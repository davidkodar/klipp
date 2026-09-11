use crate::error::RenderError;
use crate::snapshot::SequenceSnapshot;
use klipp_color::hdr::LightLevelStats;
use klipp_core::{ExportFormat, ExportSettings, ExportVideoCodec};
use std::path::Path;
use tracing::{debug, info, warn};

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
    /// Encoder output resolution (may differ from snapshot if resolution override is set).
    output_width: u32,
    output_height: u32,
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
    /// Subtitle stream index (if soft subtitle embedding is enabled).
    pub subtitle_stream_idx: Option<usize>,
    /// HDR signalling mode — drives colour-space tags at `open()` and
    /// decides whether HDR10 side-data (MaxCLL/MaxFALL + mastering
    /// display metadata) is attached at `finalise()`.
    hdr_mode: klipp_core::HdrMode,
    /// Running light-level statistics fed from every linear-light
    /// frame. MaxCLL/MaxFALL are derived from this at finalise.
    /// Only meaningful when the caller routes frames through
    /// [`Encoder::encode_linear_frame`]; pure u8-pixel exports leave
    /// this at zero and skip the HDR10 side-data.
    light_stats: LightLevelStats,
    /// Display-boundary tone-map picked by the user (or project
    /// default). Applied inside [`Encoder::encode_linear_frame`] to
    /// the linear master before sRGB encode. Read from
    /// [`ExportSettings::tone_map_op`] at `open()` time and then
    /// treated as immutable for the rest of the encode.
    tone_map_op: klipp_color::display::ToneMapOp,
    /// Delivery-target Output Display Transform. Drives the
    /// linear → display-encoded u8 step inside
    /// [`Encoder::encode_linear_frame`]. Defaults to
    /// [`OdtChoice::MatchViewer`] which mirrors the viewer's pixels
    /// pixel-for-pixel under the project's working colorspace.
    odt: klipp_core::OdtChoice,
    /// Project working colorspace, captured at `open()` time. Used
    /// alongside `odt` to dispatch the linear → display step. Read
    /// from the engine's atomic at export start so the export pixels
    /// match what the user sees in the viewer.
    working_colorspace: klipp_core::WorkingColorspace,
    /// When `Some`, this is a GIF export. The two-pass palette
    /// pipeline lives entirely behind this filter graph; the standard
    /// RGBA→YUV / audio / subtitle paths short-circuit when set.
    /// See [`crate::gif_filter`] for the palettegen/paletteuse spec.
    gif_filter: Option<crate::gif_filter::GifFilter>,
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
        ExportVideoCodec::Av1 => {
            // Try NVENC AV1 first, then SVT-AV1
            if hw_eligible {
                if ffmpeg_next::encoder::find_by_name("av1_nvenc").is_some() {
                    return ("av1_nvenc", true);
                }
            }
            ("libsvtav1", false)
        }
        ExportVideoCodec::Vp9 => ("libvpx-vp9", false),
        ExportVideoCodec::Gif => ("gif", false),
    }
}

/// Write color-space tags onto the encoder's AVCodecContext.
/// Picks BT.709 (SDR) or BT.2020 + HLG/PQ based on `settings.hdr_mode`.
/// Safety: the caller must hold a valid `&mut Encoder` context; `as_mut_ptr`
/// returns a non-null pointer for the lifetime of the context.
fn apply_color_tags(
    video_ctx: &mut ffmpeg_next::codec::encoder::video::Video,
    hdr_mode: klipp_core::HdrMode,
) {
    use ffmpeg_next::ffi::{AVColorPrimaries, AVColorSpace, AVColorTransferCharacteristic};
    let (prim, trc, space) = match hdr_mode {
        klipp_core::HdrMode::Sdr => (
            AVColorPrimaries::AVCOL_PRI_BT709,
            AVColorTransferCharacteristic::AVCOL_TRC_BT709,
            AVColorSpace::AVCOL_SPC_BT709,
        ),
        klipp_core::HdrMode::Hlg => (
            AVColorPrimaries::AVCOL_PRI_BT2020,
            AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67,
            AVColorSpace::AVCOL_SPC_BT2020_NCL,
        ),
        klipp_core::HdrMode::Pq => (
            AVColorPrimaries::AVCOL_PRI_BT2020,
            AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084,
            AVColorSpace::AVCOL_SPC_BT2020_NCL,
        ),
    };
    unsafe {
        (*video_ctx.as_mut_ptr()).color_primaries = prim;
        (*video_ctx.as_mut_ptr()).color_trc = trc;
        (*video_ctx.as_mut_ptr()).colorspace = space;
    }
}

/// x265-params string fragment matching `hdr_mode` — used by the software
/// H.265 encoder which doesn't honor the AVCodecContext fields the same way.
fn x265_color_params(hdr_mode: klipp_core::HdrMode) -> &'static str {
    match hdr_mode {
        klipp_core::HdrMode::Sdr => "colorprim=bt709:transfer=bt709:colormatrix=bt709",
        klipp_core::HdrMode::Hlg => "colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
        klipp_core::HdrMode::Pq => "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
    }
}

impl Encoder {
    /// Open the encoder for the given output path and settings.
    pub fn open(
        output_path: &Path,
        settings: &ExportSettings,
        snapshot: &SequenceSnapshot,
    ) -> Result<Self, RenderError> {
        // Force the correct container format ALWAYS — never let
        // ffmpeg auto-detect from the filename extension. This
        // breaks when the export job writes to a `.partial` sibling
        // (R-5 atomic-rename strategy in `export_job.rs`): ffmpeg
        // looks at the trailing extension and `.partial` isn't a
        // registered muxer, so the entire encoder context fails to
        // build with "Invalid argument".
        //
        // ProRes / DNxHD codecs override the user's container choice
        // because they only mux into MOV. Everything else maps
        // directly from `settings.format`.
        let forced_format = match settings.video_codec {
            ExportVideoCodec::ProRes422
            | ExportVideoCodec::ProRes422HQ
            | ExportVideoCodec::ProRes4444
            | ExportVideoCodec::DNxHD115
            | ExportVideoCodec::DNxHD145 => "mov",
            _ => match settings.format {
                ExportFormat::Mp4 => "mp4",
                ExportFormat::Mkv => "matroska",
                ExportFormat::Mov => "mov",
                ExportFormat::WebM => "webm",
                ExportFormat::Gif => "gif",
            },
        };

        let mut output_ctx = ffmpeg_next::format::output_as(output_path, forced_format)
            .map_err(|e| {
                RenderError::Encode(format!("output context (forced {forced_format}): {e}"))
            })?;

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

        // Resolution: use export override if set, else sequence resolution
        let (out_w, out_h) = match &settings.resolution {
            Some(res) => (res.width, res.height),
            None => (snapshot.width, snapshot.height),
        };
        video_ctx.set_width(out_w);
        video_ctx.set_height(out_h);
        // Frame rate: use export override if set, else sequence fps
        let (fps_num, fps_den) = match &settings.fps {
            Some(fps) => (fps.num as i32, fps.den as i32),
            None => (snapshot.fps_num as i32, snapshot.fps_den as i32),
        };
        video_ctx.set_frame_rate(Some(ffmpeg_next::Rational::new(fps_num, fps_den)));
        video_ctx.set_time_base(ffmpeg_next::Rational::new(fps_den, fps_num));

        // Set pixel format based on codec + HW/SW.
        // #65 baseline: for H.264/H.265, bit_depth == 10 selects a 10-bit
        // format. NVENC needs P010LE (semi-planar, 10 bits in MSBs of a
        // 16-bit word); software encoders take planar YUV420P10LE.
        let want_10bit = settings.bit_depth == 10;
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
                    if want_10bit {
                        ffmpeg_next::format::Pixel::P010LE
                    } else {
                        ffmpeg_next::format::Pixel::NV12
                    }
                } else if want_10bit {
                    ffmpeg_next::format::Pixel::YUV420P10LE
                } else {
                    ffmpeg_next::format::Pixel::YUV420P
                }
            }
            ExportVideoCodec::Av1 => {
                if is_hw_encoder {
                    ffmpeg_next::format::Pixel::NV12
                } else {
                    ffmpeg_next::format::Pixel::YUV420P
                }
            }
            ExportVideoCodec::Vp9 => ffmpeg_next::format::Pixel::YUV420P,
            // GIF encoder takes 8-bit palettized frames. The two-pass
            // palette path inside encode_video_frame / finalise feeds
            // the encoder with `paletteuse` filter output (PAL8).
            ExportVideoCodec::Gif => ffmpeg_next::format::Pixel::PAL8,
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
                // Colour-space tagging (BT.709 SDR or BT.2020 HDR — #65).
                apply_color_tags(&mut video_ctx, settings.hdr_mode);
            }
            ExportVideoCodec::H264 => {
                video_ctx.set_bit_rate(0); // CRF mode
                video_opts.set("crf", &settings.crf.to_string());
                video_opts.set("preset", settings.preset.ffmpeg_value());
                apply_color_tags(&mut video_ctx, settings.hdr_mode);
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
                apply_color_tags(&mut video_ctx, settings.hdr_mode);
            }
            ExportVideoCodec::H265 => {
                video_ctx.set_bit_rate(0);
                video_opts.set("crf", &settings.crf.to_string());
                video_opts.set("preset", settings.preset.ffmpeg_value());
                video_opts.set("x265-params", x265_color_params(settings.hdr_mode));
                apply_color_tags(&mut video_ctx, settings.hdr_mode);
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
            ExportVideoCodec::Av1 if is_hw_encoder => {
                video_opts.set("rc", "vbr");
                video_opts.set("cq", &settings.crf.to_string());
                video_ctx.set_bit_rate(0);
            }
            ExportVideoCodec::Av1 => {
                // SVT-AV1 uses CRF
                video_opts.set("crf", &settings.crf.to_string());
                let svt_preset = match settings.preset {
                    klipp_core::EncoderPreset::Fast => "8",
                    klipp_core::EncoderPreset::Medium => "6",
                    klipp_core::EncoderPreset::Slow => "4",
                    klipp_core::EncoderPreset::VerySlow => "2",
                };
                video_opts.set("preset", svt_preset);
            }
            ExportVideoCodec::Vp9 => {
                video_opts.set("crf", &settings.crf.to_string());
                video_opts.set("b:v", "0"); // CRF mode for VP9
                // VP9 speed/quality preset
                let speed = match settings.preset {
                    klipp_core::EncoderPreset::Fast => "4",
                    klipp_core::EncoderPreset::Medium => "2",
                    klipp_core::EncoderPreset::Slow => "1",
                    klipp_core::EncoderPreset::VerySlow => "0",
                };
                video_opts.set("speed", speed);
            }
            ExportVideoCodec::Gif => {
                // Loop count: 0 = infinite (the standard for animated
                // GIFs). Any positive value would be `loop_count - 1`
                // playbacks before stopping.
                video_opts.set("loop", "0");
            }
        }

        // Every Y'CbCr output gets its colour tags and range — before, only
        // the H.264/H.265 branches tagged colour, ProRes/DNxHD/AV1/VP9 went
        // untagged, and no codec declared its (limited) range. GIF is RGB.
        if settings.video_codec != ExportVideoCodec::Gif {
            apply_color_tags(&mut video_ctx, settings.hdr_mode);
            unsafe {
                (*video_ctx.as_mut_ptr()).color_range = ffmpeg_next::ffi::AVColorRange::AVCOL_RANGE_MPEG;
            }
        }

        // CBR/VBR override: if rate_control is not CRF and a video bitrate is set,
        // override the CRF-based settings above.
        match settings.rate_control {
            klipp_core::RateControl::Cbr => {
                if let Some(br_kbps) = settings.video_bitrate_kbps {
                    video_ctx.set_bit_rate(br_kbps as usize * 1000);
                    video_opts.set("maxrate", &format!("{}k", br_kbps));
                    video_opts.set("minrate", &format!("{}k", br_kbps));
                    video_opts.set("bufsize", &format!("{}k", br_kbps * 2));
                    // Remove CRF if it was set
                    video_opts.set("crf", "-1");
                }
            }
            klipp_core::RateControl::Vbr => {
                if let Some(br_kbps) = settings.video_bitrate_kbps {
                    video_ctx.set_bit_rate(br_kbps as usize * 1000);
                    video_opts.set("maxrate", &format!("{}k", br_kbps * 2));
                    video_opts.set("bufsize", &format!("{}k", br_kbps * 4));
                }
            }
            klipp_core::RateControl::Crf => {} // Already handled above
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

        // Create RGBA→YUV scaler.
        // When a resolution override is set, the export job applies aspect-preserving
        // fit-and-pad to produce RGBA at (out_w, out_h) before calling encode_video_frame.
        // So the scaler input matches the output — it does format conversion only, no resize.
        // When no override is set, input = output = snapshot dims (also format conversion only).
        let scaler_in_w = out_w;
        let scaler_in_h = out_h;
        let mut rgba_to_yuv = ffmpeg_next::software::scaling::Context::get(
            ffmpeg_next::format::Pixel::RGBA,
            scaler_in_w,
            scaler_in_h,
            pix_fmt,
            out_w,
            out_h,
            ffmpeg_next::software::scaling::Flags::LANCZOS,
        )
        .map_err(|e| RenderError::Encode(format!("RGBA→YUV scaler: {e}")))?;
        // Write the matrix the tags declare (swscale would use BT.601),
        // limited range as tagged above.
        if settings.video_codec != ExportVideoCodec::Gif {
            let matrix = match settings.hdr_mode {
                klipp_core::HdrMode::Sdr => klipp_core::ColorMatrix::Bt709,
                klipp_core::HdrMode::Hlg | klipp_core::HdrMode::Pq => klipp_core::ColorMatrix::Bt2020Ncl,
            };
            unsafe {
                crate::sws_colour::set_rgb_to_yuv(
                    rgba_to_yuv.as_mut_ptr() as *mut _,
                    matrix,
                    klipp_core::ColorRange::Limited,
                );
            }
        }

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
            let sample_fmt = audio_codec
                .audio()
                .ok()
                .and_then(|a| a.formats())
                .and_then(|mut fmts| fmts.next())
                .unwrap_or(ffmpeg_next::format::Sample::F32(
                    ffmpeg_next::format::sample::Type::Packed,
                ));
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
        // SUBTITLE STREAM (optional — soft-embedded text subtitles)
        // ═══════════════════════════════════════════════════════════

        let mut subtitle_stream_idx = None;

        // GIF can't carry subtitles — silently skip even when the
        // user toggled "embed subtitles". The dialog also greys the
        // option out so this is a defensive belt-and-braces gate.
        if settings.embed_subtitles
            && !snapshot.subtitle_tracks.is_empty()
            && settings.format.supports_subtitles()
        {
            let sub_codec_id = match settings.format {
                klipp_core::ExportFormat::Mp4 | klipp_core::ExportFormat::Mov => ffmpeg_next::codec::Id::MOV_TEXT,
                klipp_core::ExportFormat::Mkv => ffmpeg_next::codec::Id::SUBRIP,
                klipp_core::ExportFormat::WebM => ffmpeg_next::codec::Id::WEBVTT,
                klipp_core::ExportFormat::Gif => unreachable!("supports_subtitles guards this"),
            };

            let sub_codec = ffmpeg_next::encoder::find(sub_codec_id)
                .ok_or_else(|| RenderError::Encode(format!(
                    "subtitle codec {:?} not found in FFmpeg build", sub_codec_id
                )))?;

            let sub_tb = match settings.format {
                klipp_core::ExportFormat::Mp4 | klipp_core::ExportFormat::Mov =>
                    ffmpeg_next::Rational::new(1, 1_000_000),
                _ => ffmpeg_next::Rational::new(1, 1_000),
            };

            let mut sub_ctx = ffmpeg_next::codec::context::Context::new_with_codec(sub_codec)
                .encoder()
                .subtitle()
                .map_err(|e| RenderError::Encode(format!("subtitle encoder context: {e}")))?;

            // Set time_base, width, and height on the encoder context before opening.
            // mov_text requires width/height (display region for rendered text).
            let global_header = output_ctx
                .format()
                .flags()
                .contains(ffmpeg_next::format::flag::Flags::GLOBAL_HEADER);
            // FFmpeg subtitle encoders (mov_text, subrip, webvtt) all require a valid
            // ASS subtitle_header on the codec context — normally provided by a decoder,
            // but for programmatic encoding from text we must set it manually.
            const ASS_HEADER: &[u8] = b"[Script Info]\nScriptType: v4.00+\nPlayResX: 384\nPlayResY: 288\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,16,&Hffffff,&Hffffff,&H0,&H0,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,0\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

            unsafe {
                let p = sub_ctx.as_mut_ptr();
                (*p).time_base = sub_tb.into();
                (*p).width = snapshot.width as i32;
                (*p).height = snapshot.height as i32;
                if global_header {
                    (*p).flags |= ffmpeg_next::ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
                }
                // Allocate and copy ASS header (FFmpeg owns this memory after open)
                let header_buf = ffmpeg_next::ffi::av_mallocz(ASS_HEADER.len() + 1) as *mut u8;
                if header_buf.is_null() {
                    return Err(RenderError::Encode("av_mallocz failed for subtitle header".into()));
                }
                std::ptr::copy_nonoverlapping(ASS_HEADER.as_ptr(), header_buf, ASS_HEADER.len());
                (*p).subtitle_header = header_buf;
                (*p).subtitle_header_size = ASS_HEADER.len() as i32;
            }

            let sub_encoder = sub_ctx
                .open_as(sub_codec)
                .map_err(|e| RenderError::Encode(format!("open subtitle encoder: {e:?}")))?;

            let s_idx = unsafe {
                let stream_ptr = ffmpeg_next::ffi::avformat_new_stream(
                    output_ctx.as_mut_ptr(),
                    std::ptr::null(),
                );
                if stream_ptr.is_null() {
                    return Err(RenderError::Encode("failed to add subtitle stream".into()));
                }
                ffmpeg_next::ffi::avcodec_parameters_from_context(
                    (*stream_ptr).codecpar,
                    sub_encoder.as_ptr(),
                );
                (*stream_ptr).time_base = sub_tb.into();
                ((*output_ctx.as_ptr()).nb_streams - 1) as usize
            };

            subtitle_stream_idx = Some(s_idx);
            info!("Encoder: subtitle stream {} ({:?})", s_idx, sub_codec_id);
        }

        // ═══════════════════════════════════════════════════════════
        // CHAPTERS (optional — MP4/MKV/MOV only; WebM has no chapters)
        // ═══════════════════════════════════════════════════════════
        //
        // Must be added BEFORE write_header() — the muxer freezes the
        // chapter list into the container's opening metadata box.
        // ffmpeg-next 8.1 exposes the safe `Output::add_chapter(id, tb,
        // start, end, title)` helper (format/context/output.rs:112-170),
        // which wraps the av_mallocz + av_dynarray_add + set_metadata
        // dance internally. No unsafe needed here.
        if !snapshot.chapters.is_empty() {
            let supports_chapters = matches!(
                settings.format,
                klipp_core::ExportFormat::Mp4
                    | klipp_core::ExportFormat::Mkv
                    | klipp_core::ExportFormat::Mov
            );
            if supports_chapters {
                let tb = ffmpeg_next::Rational::new(1, 1000);
                for (i, ch) in snapshot.chapters.iter().enumerate() {
                    if let Err(e) = output_ctx.add_chapter(
                        i as i64,
                        tb,
                        ch.start_ms as i64,
                        ch.end_ms as i64,
                        ch.title.as_str(),
                    ) {
                        warn!(
                            "Encoder: add_chapter {}/{} failed ({}), skipping",
                            i + 1,
                            snapshot.chapters.len(),
                            e
                        );
                    }
                }
                info!(
                    "Encoder: embedded {} chapter{}",
                    snapshot.chapters.len(),
                    if snapshot.chapters.len() == 1 { "" } else { "s" }
                );
            } else {
                debug!(
                    "Encoder: {:?} does not support chapters — skipping {} entries",
                    settings.format,
                    snapshot.chapters.len()
                );
            }
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
            output_width: out_w,
            output_height: out_h,
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
            subtitle_stream_idx,
            hdr_mode: settings.hdr_mode,
            light_stats: LightLevelStats::new(),
            tone_map_op: settings.tone_map_op,
            odt: settings.odt,
            // Captured from the snapshot at open() — the export-job
            // dispatch is responsible for populating
            // `snapshot.working_colorspace` from the project at the
            // moment the export starts. Pinning it here means a
            // mid-export flip of the project setting can't make
            // pixels diverge across frames.
            working_colorspace: snapshot.working_colorspace,
            gif_filter: if matches!(settings.format, ExportFormat::Gif) {
                let fps_den = (snapshot.fps.max(1.0).round() as u32).max(1);
                Some(crate::gif_filter::GifFilter::new(out_w, out_h, fps_den)?)
            } else {
                None
            },
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

    /// Encode one linear-light f32 RGBA video frame.
    ///
    /// For SDR exports this is a straightforward linear → sRGB u8
    /// encode via the display transform followed by the existing
    /// RGBA→YUV scaler. The tone-map operator is user-configurable
    /// via [`ExportSettings::tone_map_op`]: identity (`None`) for
    /// "render what the grade produced", or filmic curves (`Hable`,
    /// `Reinhard`, `Aces`) when the linear master legitimately
    /// contains HDR values that need rolling off before encode.
    ///
    /// For HDR exports (`HdrMode::Hlg`, `HdrMode::Pq`) this currently
    /// falls back to the sRGB path so the exported file has *correct*
    /// colour-space tags ([`apply_color_tags`] writes BT.2020 + the
    /// chosen transfer characteristic) but the pixel values
    /// themselves are still sRGB-encoded. A follow-up phase wires in
    /// a genuine PQ/HLG transfer encode so 10-bit HDR pixel data
    /// reaches the file.
    ///
    /// HDR10 signalling side-data (MaxCLL / MaxFALL + mastering
    /// display metadata) is populated from linear-light statistics
    /// accumulated here and attached to the stream at
    /// [`Self::finalise`] when `hdr_mode != HdrMode::Sdr`.
    pub fn encode_linear_frame(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
    ) -> Result<(), RenderError> {
        // Accumulate light-level statistics from the linear master
        // before we quantise to display u8 — once we've clipped at 1.0
        // we can no longer recover the HDR peak. Cheap enough to run
        // unconditionally; `LightLevelStats` fits in three f64s.
        self.light_stats.update(linear_rgba);
        // Route through the delivery-target Output Display Transform.
        // For `OdtChoice::MatchViewer` this dispatches based on the
        // project working colorspace and matches the viewer's pixels
        // pixel-for-pixel; explicit ODTs (Rec.709 SDR / Rec.2100 PQ /
        // DCI-P3) walk the linear master into AcesCG first when the
        // project's working space is BT.709-linear so the ODT chain
        // sees the right basis.
        let display_u8 = klipp_color::display::display_transform_for_export(
            linear_rgba,
            self.tone_map_op,
            self.working_colorspace,
            self.odt,
        );
        self.encode_video_frame(&display_u8, width, height)
    }

    /// Encode one RGBA video frame.
    pub fn encode_video_frame(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), RenderError> {
        // GIF mode short-circuit: route the RGBA bytes through the
        // palettegen+paletteuse filter graph instead of the YUV path.
        // Output frames trickle out only after end-of-stream because
        // `palettegen=stats_mode=full` waits for every frame before
        // emitting its global palette — that's why the body of this
        // call typically returns before pushing anything to the gif
        // encoder, and the bulk of the work happens in `finalise`.
        if self.gif_filter.is_some() {
            return self.encode_video_frame_gif(rgba);
        }
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

        // Convert RGBA → YUV using pre-created scaler (may resize if export res differs from snapshot)
        let mut yuv_frame = ffmpeg_next::frame::Video::new(
            self.video_encoder.format(),
            self.output_width,
            self.output_height,
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
        if self.audio_encoder.is_none() || self.gif_filter.is_some() {
            // GIF has no audio — silently drop. The export dialog
            // also disables the audio settings UI when format is GIF.
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
                    // Write both planes via raw pointers to ensure we hit the
                    // actual AVFrame data planes (not ffmpeg-next's wrapper which
                    // may return incorrect slice lengths for plane 1).
                    unsafe {
                        let ptr = frame.as_mut_ptr();
                        let p0 = (*ptr).data[0] as *mut f32;
                        let p1 = (*ptr).data[1] as *mut f32;
                        if !p0.is_null() && !p1.is_null() {
                            for i in 0..frame_size {
                                *p0.add(i) = self.audio_buffer[i * 2];
                                *p1.add(i) = self.audio_buffer[i * 2 + 1];
                            }
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

            // Get raw pointers before borrowing self for send/drain
            let (frame_ptr, enc_ptr) = unsafe {
                let fp = match &self.audio_frame {
                    Some(f) => f.as_ptr(),
                    None => continue,
                };
                let ep = self.audio_encoder.as_mut().unwrap().as_mut_ptr();
                (fp, ep)
            };

            // Send frame via raw FFI (avoids borrow conflicts with drain).
            // Advance audio_pts only on success — previously it was
            // incremented unconditionally before the send, so a failed
            // send still shifted the PTS timeline forward, producing
            // silent audio drift in the exported file.
            let send_result =
                unsafe { ffmpeg_next::ffi::avcodec_send_frame(enc_ptr, frame_ptr) };
            match send_result {
                0 => {
                    self.audio_pts += frame_size as i64;
                }
                e if e == ffmpeg_next::error::EAGAIN => {
                    self.drain_audio_packets()?;
                    let ep2 = unsafe { self.audio_encoder.as_mut().unwrap().as_mut_ptr() };
                    let retry =
                        unsafe { ffmpeg_next::ffi::avcodec_send_frame(ep2, frame_ptr) };
                    if retry == 0 {
                        self.audio_pts += frame_size as i64;
                    } else {
                        return Err(RenderError::Encode(format!(
                            "audio send_frame failed after EAGAIN drain (error={retry}) at pts={}",
                            self.audio_pts,
                        )));
                    }
                }
                e => {
                    return Err(RenderError::Encode(format!(
                        "audio send_frame error={e} at pts={}",
                        self.audio_pts,
                    )));
                }
            }

            self.drain_audio_packets()?;
        }

        Ok(())
    }

    /// Write a pre-built subtitle packet to the output.
    /// The packet must have stream_index, pts, dts, and duration already set
    /// in the subtitle stream's time_base.
    #[allow(unused_mut)]
    pub fn write_subtitle_packet(&mut self, mut packet: ffmpeg_next::Packet) -> Result<(), RenderError> {
        packet
            .write_interleaved(&mut self.output_ctx)
            .map_err(|e| RenderError::Encode(format!("write subtitle packet: {e}")))?;
        Ok(())
    }

    /// Finalise the output file. Must be called after all frames are encoded.
    pub fn finalise(mut self) -> Result<(), RenderError> {
        // GIF mode: flush the filter graph (this is when palettegen
        // emits its global palette and paletteuse drains all the
        // buffered video frames as PAL8), feed each through the gif
        // encoder, then write the trailer. No HDR side-data, no audio
        // flush, no MaxCLL — those concepts don't apply to GIF.
        if let Some(mut gif) = self.gif_filter.take() {
            let frames = gif.finish()?;
            for mut frame in frames {
                frame.set_pts(Some(self.video_pts));
                self.video_pts += 1;
                self.video_encoder
                    .send_frame(&frame)
                    .map_err(|e| RenderError::Encode(format!("gif send_frame: {e}")))?;
                self.drain_video_packets()?;
            }
            self.video_encoder
                .send_eof()
                .map_err(|e| RenderError::Encode(format!("gif send_eof: {e}")))?;
            self.drain_video_packets()?;
            self.output_ctx
                .write_trailer()
                .map_err(|e| RenderError::Encode(format!("gif write trailer: {e}")))?;
            debug!("Encoder: GIF finalised ({} frames)", self.video_pts);
            return Ok(());
        }
        // Flush video encoder
        self.video_encoder
            .send_eof()
            .map_err(|e| RenderError::Encode(format!("video send_eof: {e}")))?;
        self.drain_video_packets()?;

        // HDR10 signalling side-data has to land on the video
        // stream's codecpar before write_trailer: for MP4/MOV the
        // muxer writes the `clli` and `mdcv` atoms inside the moov
        // box at trailer time, reading from codecpar->coded_side_data.
        // Attach now, once the per-frame stats are final.
        if !matches!(self.hdr_mode, klipp_core::HdrMode::Sdr) {
            if let Err(e) = self.attach_hdr10_side_data() {
                warn!(
                    "HDR10 side-data attach failed ({}); file will be tagged \
                     BT.2020/{} but MaxCLL/MaxFALL/mastering display missing",
                    e,
                    match self.hdr_mode {
                        klipp_core::HdrMode::Hlg => "HLG",
                        klipp_core::HdrMode::Pq => "PQ",
                        klipp_core::HdrMode::Sdr => "SDR",
                    }
                );
            }
        }

        // Flush remaining audio buffer + encoder
        if self.audio_encoder.is_some() {
            // Encode any remaining partial audio frame
            let frame_size = self.audio_frame_size.unwrap_or(1024);
            let channels = 2usize;
            if !self.audio_buffer.is_empty() {
                let floats_per_frame = frame_size * channels;
                // Zero-pad the trailing partial frame up to the codec's
                // required block size. The encoder below reads directly
                // from `self.audio_buffer`, so no copy is needed.
                self.audio_buffer.resize(floats_per_frame, 0.0);
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
                        unsafe {
                            let ptr = frame.as_mut_ptr();
                            let p0 = (*ptr).data[0] as *mut f32;
                            let p1 = (*ptr).data[1] as *mut f32;
                            if !p0.is_null() && !p1.is_null() {
                                for i in 0..frame_size {
                                    *p0.add(i) = self.audio_buffer.get(i * 2).copied().unwrap_or(0.0);
                                    *p1.add(i) = self.audio_buffer.get(i * 2 + 1).copied().unwrap_or(0.0);
                                }
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

    /// GIF-mode video frame ingest. Pushes the RGBA bytes through
    /// the palettegen+paletteuse filter graph and forwards any
    /// completed PAL8 frames into the gif encoder. The graph buffers
    /// internally until end-of-stream because palettegen needs every
    /// frame before emitting its global palette — so the bulk of
    /// drained frames typically come back during `finalise`.
    fn encode_video_frame_gif(&mut self, rgba: &[u8]) -> Result<(), RenderError> {
        let frames = self
            .gif_filter
            .as_mut()
            .expect("encode_video_frame_gif called without gif_filter")
            .push_frame(rgba)?;
        for mut frame in frames {
            frame.set_pts(Some(self.video_pts));
            self.video_pts += 1;
            self.video_encoder
                .send_frame(&frame)
                .map_err(|e| RenderError::Encode(format!("gif send_frame: {e}")))?;
            self.drain_video_packets()?;
        }
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

    /// Attach HDR10 signalling side-data (Content Light Level +
    /// Mastering Display Colour Volume) to the video stream.
    ///
    /// Called once, at finalise time, when `hdr_mode != Sdr`. For MP4/
    /// MOV the MOV muxer translates these into the `clli` and `mdcv`
    /// sample-description atoms; for MKV they become container-level
    /// `ColourMasteringMetadata` / `MaxCLL` / `MaxFALL` elements.
    ///
    /// Uses `av_packet_side_data_add` (new API in FFmpeg 6+), which
    /// takes ownership of an `av_malloc`'d buffer. On success the
    /// buffer will be freed when `avcodec_parameters_free` runs during
    /// output-context teardown; on failure we retain ownership and
    /// free it ourselves.
    fn attach_hdr10_side_data(&mut self) -> Result<(), RenderError> {
        // Locally-declared C layouts matching libavutil's
        // `mastering_display_metadata.h`. ffmpeg-sys-next's bindgen
        // step does not emit these structs, so we describe them here.
        // If the C ABI ever changes this would need to move in lock-
        // step, but these layouts have been stable since the HDR10
        // support landed in FFmpeg ~3.3 (2017).
        #[repr(C)]
        #[derive(Copy, Clone)]
        struct AvContentLightMetadata {
            max_cll: std::os::raw::c_uint,
            max_fall: std::os::raw::c_uint,
        }

        #[repr(C)]
        #[derive(Copy, Clone)]
        struct AvMasteringDisplayMetadata {
            display_primaries: [[ffmpeg_next::ffi::AVRational; 2]; 3],
            white_point: [ffmpeg_next::ffi::AVRational; 2],
            min_luminance: ffmpeg_next::ffi::AVRational,
            max_luminance: ffmpeg_next::ffi::AVRational,
            has_primaries: std::os::raw::c_int,
            has_luminance: std::os::raw::c_int,
        }

        // Rationalise a CIE xy coord at 1/50000 resolution — the
        // CTA-861.3 / H.265 wire format. 0.3127 → 15635 / 50000.
        fn xy_rat(v: f32) -> ffmpeg_next::ffi::AVRational {
            ffmpeg_next::ffi::AVRational {
                num: (v * 50_000.0).round() as std::os::raw::c_int,
                den: 50_000,
            }
        }

        // Luminance at 0.0001 cd/m² resolution — same wire format.
        fn lum_rat(cd_m2: f32) -> ffmpeg_next::ffi::AVRational {
            ffmpeg_next::ffi::AVRational {
                num: (cd_m2 * 10_000.0).round() as std::os::raw::c_int,
                den: 10_000,
            }
        }

        use klipp_color::hdr::{
            bt2020_primaries, DEFAULT_MASTERING_MAX_LUMA_CD_M2, DEFAULT_MASTERING_MIN_LUMA_CD_M2,
        };

        let cll_payload = AvContentLightMetadata {
            max_cll: self.light_stats.max_cll_nits() as std::os::raw::c_uint,
            max_fall: self.light_stats.max_fall_nits() as std::os::raw::c_uint,
        };

        let mastering_payload = AvMasteringDisplayMetadata {
            display_primaries: [
                [
                    xy_rat(bt2020_primaries::RED.0),
                    xy_rat(bt2020_primaries::RED.1),
                ],
                [
                    xy_rat(bt2020_primaries::GREEN.0),
                    xy_rat(bt2020_primaries::GREEN.1),
                ],
                [
                    xy_rat(bt2020_primaries::BLUE.0),
                    xy_rat(bt2020_primaries::BLUE.1),
                ],
            ],
            white_point: [
                xy_rat(bt2020_primaries::WHITE_D65.0),
                xy_rat(bt2020_primaries::WHITE_D65.1),
            ],
            min_luminance: lum_rat(DEFAULT_MASTERING_MIN_LUMA_CD_M2),
            max_luminance: lum_rat(DEFAULT_MASTERING_MAX_LUMA_CD_M2),
            has_primaries: 1,
            has_luminance: 1,
        };

        unsafe {
            // Get a mutable pointer to the video stream's codecpar so
            // we can attach side data directly. The muxer reads from
            // codecpar.coded_side_data at write_trailer time for MP4.
            let out_ptr = self.output_ctx.as_mut_ptr();
            if out_ptr.is_null() {
                return Err(RenderError::Encode(
                    "output context pointer is null".into(),
                ));
            }
            let stream_ptr = *(*out_ptr).streams.add(self.video_stream_idx);
            if stream_ptr.is_null() {
                return Err(RenderError::Encode(
                    "video stream pointer is null".into(),
                ));
            }
            let par = (*stream_ptr).codecpar;
            if par.is_null() {
                return Err(RenderError::Encode(
                    "video codecpar pointer is null".into(),
                ));
            }

            // Copy payloads into av_malloc'd buffers — ownership
            // transfers to FFmpeg on a successful add.
            let cll_size = std::mem::size_of::<AvContentLightMetadata>();
            let cll_buf = ffmpeg_next::ffi::av_mallocz(cll_size) as *mut AvContentLightMetadata;
            if cll_buf.is_null() {
                return Err(RenderError::Encode(
                    "av_mallocz AVContentLightMetadata".into(),
                ));
            }
            *cll_buf = cll_payload;

            let mastering_size = std::mem::size_of::<AvMasteringDisplayMetadata>();
            let mastering_buf =
                ffmpeg_next::ffi::av_mallocz(mastering_size) as *mut AvMasteringDisplayMetadata;
            if mastering_buf.is_null() {
                // Free the CLL buffer we already allocated — we're
                // about to bail out and won't be attaching either.
                ffmpeg_next::ffi::av_free(cll_buf as *mut std::os::raw::c_void);
                return Err(RenderError::Encode(
                    "av_mallocz AVMasteringDisplayMetadata".into(),
                ));
            }
            *mastering_buf = mastering_payload;

            // AV_PKT_DATA_CONTENT_LIGHT_LEVEL = 22, AV_PKT_DATA_MASTERING_DISPLAY_METADATA = 20
            // — matches the enum values in bindings.rs (lines 6660,
            // 6664). Same values have held since FFmpeg 3.3.
            let cll_ret = ffmpeg_next::ffi::av_packet_side_data_add(
                &mut (*par).coded_side_data as *mut *mut _,
                &mut (*par).nb_coded_side_data as *mut _,
                ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_CONTENT_LIGHT_LEVEL,
                cll_buf as *mut std::os::raw::c_void,
                cll_size,
                0,
            );
            if cll_ret.is_null() {
                ffmpeg_next::ffi::av_free(cll_buf as *mut std::os::raw::c_void);
                ffmpeg_next::ffi::av_free(mastering_buf as *mut std::os::raw::c_void);
                return Err(RenderError::Encode(
                    "av_packet_side_data_add CONTENT_LIGHT_LEVEL failed".into(),
                ));
            }

            let mastering_ret = ffmpeg_next::ffi::av_packet_side_data_add(
                &mut (*par).coded_side_data as *mut *mut _,
                &mut (*par).nb_coded_side_data as *mut _,
                ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_MASTERING_DISPLAY_METADATA,
                mastering_buf as *mut std::os::raw::c_void,
                mastering_size,
                0,
            );
            if mastering_ret.is_null() {
                // CLL was already handed to FFmpeg; the mastering
                // buffer is still ours to free.
                ffmpeg_next::ffi::av_free(mastering_buf as *mut std::os::raw::c_void);
                return Err(RenderError::Encode(
                    "av_packet_side_data_add MASTERING_DISPLAY_METADATA failed".into(),
                ));
            }
        }

        info!(
            "HDR10 metadata attached: MaxCLL={} nits, MaxFALL={} nits ({} frames sampled)",
            self.light_stats.max_cll_nits(),
            self.light_stats.max_fall_nits(),
            self.light_stats.frame_count(),
        );

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
    use serial_test::serial;

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
            audio_bitrate_kbps: Some(128), video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, embed_chapters: false,
            bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
            odt: klipp_core::OdtChoice::MatchViewer }
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
            subtitle_tracks: vec![], chapters: vec![],
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        }
    }

    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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

    /// End-to-end: exporting linear frames with `HdrMode::Pq` must
    /// attach HDR10 side-data to the output file (Content Light Level
    /// + Mastering Display Colour Volume). MaxCLL lands on a
    /// 1000-nit highlight pixel as the round-trip acceptance for
    /// Phase 11's metadata polish.
    ///
    /// Requires ffprobe on PATH; if not available the test skips its
    /// assertion block (same pattern as `encode_h264_10_frames`).
    #[test]
    #[serial]
    fn hdr10_side_data_attached_for_pq_export() {
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_hdr10.mp4");

        let mut settings = make_settings(ExportVideoCodec::H265, path.to_str().unwrap());
        settings.audio_codec = ExportAudioCodec::Copy;
        settings.hdr_mode = klipp_core::HdrMode::Pq;
        settings.bit_depth = 10;

        // 320×240 synthetic "master" with one 1000-nit highlight
        // pixel (10.0 linear) and the rest diffuse white (1.0).
        let (w, h) = (320usize, 240usize);
        let snapshot = SequenceSnapshot {
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: w as u32,
            height: h as u32,
            sample_rate: 48000,
            duration_frames: 4,
            export_range: None,
            video_tracks: vec![],
            audio_tracks: vec![],
            global_lut_path: None,
            subtitle_tracks: vec![],
            chapters: vec![],
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        };

        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();

        let mut frame = vec![1.0_f32; w * h * 4];
        for px in frame.chunks_exact_mut(4) {
            px[3] = 1.0;
        }
        frame[0] = 10.0;
        frame[1] = 10.0;
        frame[2] = 10.0;
        for _ in 0..4 {
            enc.encode_linear_frame(&frame, w as u32, h as u32).unwrap();
        }

        enc.finalise().unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 100, "HDR10 output file should be non-trivial");

        // Verify HDR10 side-data via ffprobe. We look for `MaxCLL`
        // and `Mastering display metadata` in the stream-level
        // side_data_list (ffprobe surfaces both under -show_streams).
        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v", "error",
                "-select_streams", "v:0",
                "-show_frames",
                "-read_intervals", "%+#1",
                "-show_entries", "frame=side_data_list",
                "-of", "json",
            ])
            .arg(&path)
            .output();

        if let Ok(out) = probe {
            let json = String::from_utf8_lossy(&out.stdout);
            // Stream-level side_data probe: ask for the streams view
            // as well, since MP4's clli/mdcv surface as per-stream
            // side_data rather than per-frame.
            let stream_probe = std::process::Command::new("ffprobe")
                .args([
                    "-v", "error",
                    "-select_streams", "v:0",
                    "-show_entries", "stream_side_data_list",
                    "-of", "json",
                ])
                .arg(&path)
                .output()
                .ok();
            let stream_json = stream_probe
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();

            let combined = format!("{json}\n{stream_json}");
            assert!(
                combined.contains("Content light level metadata")
                    || combined.contains("MaxCLL"),
                "expected MaxCLL side-data in ffprobe output, got: {combined}",
            );
            assert!(
                combined.contains("Mastering display metadata")
                    || combined.contains("mastering_display_metadata"),
                "expected mastering-display side-data in ffprobe output, got: {combined}",
            );
            assert!(
                combined.contains("1000"),
                "expected MaxCLL=1000 somewhere in side-data, got: {combined}",
            );
        }

        let _ = std::fs::remove_file(&path);
    }
}
