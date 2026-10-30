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
    audio_bitrate_kbps: Option<u32>,
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
    /// RGBA64 → YUV scaler for 10-bit outputs, made on first use.
    rgba64_to_yuv: Option<ffmpeg_next::software::scaling::Context>,
    /// The Y'CbCr matrix both scalers write (the one the file is tagged).
    yuv_matrix: klipp_core::ColorMatrix,
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
    /// Name of the audio encoder in use, when there is sound (#1924).
    audio_codec_name: Option<&'static str>,
    /// The pictures' layout in memory. The encoder's own format, except
    /// for VAAPI, which takes surfaces and is sent these uploaded.
    frame_format: ffmpeg_next::format::Pixel,
    /// The first hardware encoder that could not open, when the render
    /// fell back to software (#381).
    hardware_fallback: Option<&'static str>,
    /// Audio resampler (f32 packed stereo → encoder's required format).
    /// Reserved for future use when format conversion is needed beyond FLTP.
    #[allow(dead_code)]
    audio_resampler: Option<ffmpeg_next::software::resampling::Context>,
    /// The encoder's actual sample format.
    audio_sample_format: Option<ffmpeg_next::format::Sample>,
    /// Subtitle stream index (if soft subtitle embedding is enabled).
    pub subtitle_stream_idx: Option<usize>,
    /// The file's transfer ([`ExportSettings::delivery`]) — drives the
    /// colour-space tags at `open()` and, for PQ, the HDR10 side data
    /// (MaxCLL/MaxFALL + mastering display) attached at `finalise()`.
    hdr_mode: klipp_core::HdrMode,
    /// MaxCLL/MaxFALL of a PQ delivery, measured on the signal written.
    /// Only frames through [`Encoder::encode_linear_frame`] count.
    light_stats: LightLevelStats,
    /// The mastering display's peak, cd/m², for the HDR10 side data.
    hdr_peak_nits: f32,
    /// The subtitle track as CEA-608 captions in the video stream
    /// (#1663), when the export carries them.
    captions: Option<crate::a53::Captions>,
    /// HDR10+ dynamic metadata, per shot (#468), when the export carries it.
    hdr10plus: Option<crate::hdr10plus::Hdr10Plus>,
    /// The colour tags every picture is sent with, limited range: an
    /// encoder that writes each frame's own (ProRes, in its frame header)
    /// writes these. `None` for a GIF, which is RGB.
    frame_tags: Option<ColorTags>,
    /// Linear picture → display-encoded RGB, per pixel: the viewer's
    /// transform, an ACES output transform, or an HDR delivery's PQ/HLG
    /// encode. Fixed at `open()` from the settings and the project's
    /// working colorspace, so a mid-export change cannot make frames
    /// disagree.
    display: klipp_color::display::ExportDisplay,
    /// When `Some`, this is a GIF export. The two-pass palette
    /// pipeline lives entirely behind this filter graph; the standard
    /// RGBA→YUV / audio / subtitle paths short-circuit when set.
    /// See [`crate::gif_filter`] for the palettegen/paletteuse spec.
    gif_filter: Option<crate::gif_filter::GifFilter>,
}

/// One encoder a render may try, in the order [`video_encoder_attempts`]
/// lists them.
#[derive(Debug, Clone, PartialEq)]
struct EncoderAttempt {
    name: &'static str,
    /// On the GPU (NVENC, AMF or VAAPI) rather than the CPU.
    hardware: bool,
    /// For a VAAPI encoder: the render node, and whether to ask for the
    /// low-power entrypoint, which is the only one some Intel GPUs have
    /// for HEVC and AV1.
    vaapi: Option<(std::path::PathBuf, bool)>,
}

/// The hardware encoders this FFmpeg has for `codec`: NVENC (NVIDIA), then
/// AMF (AMD), then VAAPI (AMD and Intel).
///
/// A distribution's FFmpeg is built with all three whatever the machine
/// has, so being listed here says nothing about the GPU: it is
/// `avcodec_open2` that finds out, and the render then moves on to the
/// next. NVENC needs at least 128×128, and the others are held to the
/// same.
///
/// `want_10bit` matters for H.264 and only H.264: **no hardware H.264
/// encoder does ten bits.** NVENC's and AMF's H.264 blocks are 8-bit
/// 4:2:0, while their HEVC and AV1 blocks are not. FFmpeg is no help in
/// telling them apart — all three nvenc encoders advertise one shared
/// pixel-format list with `p010le` in it, and the real capability check
/// happens inside `avcodec_open2`, which answers "No capable devices
/// found" and `AVERROR_EXTERNAL`. That surfaces to the user as "encoder
/// error: open video encoder: Generic error in an external library" on a
/// render that had every reason to work, which is what this parameter
/// exists to prevent: a 10-bit H.264 export goes to `libx264`, whose High
/// 10 profile does it.
///
/// Since the export UI only offers HDR at ten bits, every HDR H.264
/// render hit this.
fn hardware_video_encoders(codec: &ExportVideoCodec, width: u32, height: u32, want_10bit: bool) -> Vec<&'static str> {
    if width < 128 || height < 128 {
        return Vec::new();
    }
    let names: &[&'static str] = match codec {
        ExportVideoCodec::H264 if !want_10bit => &["h264_nvenc", "h264_amf", "h264_vaapi"],
        ExportVideoCodec::H265 => &["hevc_nvenc", "hevc_amf", "hevc_vaapi"],
        ExportVideoCodec::Av1 => &["av1_nvenc", "av1_amf", "av1_vaapi"],
        _ => &[],
    };
    names.iter().copied().filter(|name| ffmpeg_next::encoder::find_by_name(name).is_some()).collect()
}

/// Every encoder a render tries for `codec`, best first: each hardware
/// encoder in [`hardware_video_encoders`], VAAPI once per node in
/// `vaapi_nodes` (and again there in low power), then the software one.
fn video_encoder_attempts(
    codec: &ExportVideoCodec,
    width: u32,
    height: u32,
    want_10bit: bool,
    vaapi_nodes: &[std::path::PathBuf],
) -> Vec<EncoderAttempt> {
    let mut attempts = Vec::new();
    for name in hardware_video_encoders(codec, width, height, want_10bit) {
        if name.ends_with("_vaapi") {
            for node in vaapi_nodes {
                for low_power in [false, true] {
                    attempts.push(EncoderAttempt { name, hardware: true, vaapi: Some((node.clone(), low_power)) });
                }
            }
        } else {
            attempts.push(EncoderAttempt { name, hardware: true, vaapi: None });
        }
    }
    attempts.push(EncoderAttempt { name: software_video_encoder(codec), hardware: false, vaapi: None });
    attempts
}

/// The encoder a render on this machine tries first, and whether it is
/// hardware.
#[cfg(test)]
fn pick_video_encoder(codec: &ExportVideoCodec, width: u32, height: u32, want_10bit: bool) -> (&'static str, bool) {
    let first = video_encoder_attempts(codec, width, height, want_10bit, &crate::vaapi::encode_nodes()).remove(0);
    (first.name, first.hardware)
}

/// A hardware encoder's constant-quality settings, from the software
/// encoders' CRF number. Each family spells it differently: NVENC as VBR
/// with a `cq` target, AMF and VAAPI as a constant QP. AV1's quantiser
/// runs to 255 on AMF and VAAPI, so its CRF (to 63) is scaled up.
fn hardware_quality(
    name: &str,
    settings: &ExportSettings,
    video_ctx: &mut ffmpeg_next::codec::encoder::video::Video,
    video_opts: &mut ffmpeg_next::Dictionary,
) {
    video_ctx.set_bit_rate(0);
    let av1 = settings.video_codec == ExportVideoCodec::Av1;
    let qp = if av1 { (settings.crf * 4).min(255) } else { settings.crf };
    if name.ends_with("_nvenc") {
        // NVENC: use VBR with constant quality (cq) — equivalent to CRF
        video_opts.set("rc", "vbr");
        video_opts.set("cq", &settings.crf.to_string());
        if !av1 {
            // Set a high bitrate cap so VBR has room (0 = uncapped)
            video_opts.set("b:v", "0");
            // NVENC preset: p1(fastest)..p7(best quality)
            let nvenc_preset = match settings.preset {
                klipp_core::EncoderPreset::Fast => "p4",
                klipp_core::EncoderPreset::Medium => "p5",
                klipp_core::EncoderPreset::Slow => "p6",
                klipp_core::EncoderPreset::VerySlow => "p7",
            };
            video_opts.set("preset", nvenc_preset);
        }
    } else if name.ends_with("_amf") {
        // AMF refuses NVENC's `rc vbr` and `preset p5` outright, so the
        // encoder never opened when it was given them.
        video_opts.set("rc", "cqp");
        video_opts.set("qp_i", &qp.to_string());
        video_opts.set("qp_p", &qp.to_string());
        video_opts.set("qp_b", &qp.to_string());
        let amf_preset = match settings.preset {
            klipp_core::EncoderPreset::Fast => "speed",
            klipp_core::EncoderPreset::Medium => "balanced",
            klipp_core::EncoderPreset::Slow => "quality",
            klipp_core::EncoderPreset::VerySlow => "high_quality",
        };
        video_opts.set("preset", amf_preset);
    } else if name.ends_with("_vaapi") {
        video_opts.set("rc_mode", "CQP");
        video_ctx.set_global_quality(qp as i32);
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: make hardware encoders refuse to open, as when all of a
    /// card's NVENC sessions are taken, and optionally software ones too.
    static FAIL_HARDWARE_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_SOFTWARE_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: the encoders to try, in place of this machine's.
    static ATTEMPTS: std::cell::RefCell<Option<Vec<EncoderAttempt>>> = const { std::cell::RefCell::new(None) };
}

/// The software encoder for `codec`: what a render falls back to when the
/// hardware encoders cannot open.
fn software_video_encoder(codec: &ExportVideoCodec) -> &'static str {
    match codec {
        ExportVideoCodec::H264 => "libx264",
        ExportVideoCodec::H265 => "libx265",
        ExportVideoCodec::Av1 => "libsvtav1",
        ExportVideoCodec::ProRes422 | ExportVideoCodec::ProRes422HQ | ExportVideoCodec::ProRes4444 => "prores_ks",
        ExportVideoCodec::DNxHD115 | ExportVideoCodec::DNxHD145 => "dnxhd",
        ExportVideoCodec::Vp9 => "libvpx-vp9",
        ExportVideoCodec::Gif => "gif",
    }
}

/// The colour tags an export carries, in FFmpeg's and x265's terms.
#[derive(Clone, Copy)]
struct ColorTags {
    primaries: ffmpeg_next::ffi::AVColorPrimaries,
    transfer: ffmpeg_next::ffi::AVColorTransferCharacteristic,
    matrix: ffmpeg_next::ffi::AVColorSpace,
    x265: &'static str,
}

/// The tags for what the export writes: BT.2020 with HLG or PQ for an
/// HDR delivery, and for SDR the output transform actually applied. The
/// DCI-P3 D65 transform writes P3 primaries with a pure 2.6 gamma, which
/// has no transfer code (SMPTE 428 is the XYZ encoding), so its transfer
/// is left unspecified rather than claimed as BT.709. Every other SDR
/// export says BT.709, the sRGB-curve ones included: Match Viewer in a
/// Rec.709 or BT.2020 project writes the same curve as the sRGB transform,
/// and the two must not be tagged apart. The matrix is the one the
/// scalers convert with (BT.709 for SDR, BT.2020 for Rec.2020 SDR), since
/// a P3 file is still Y'CbCr by the BT.709 matrix.
fn color_tags(delivery: klipp_core::HdrMode, odt: klipp_core::OdtChoice) -> ColorTags {
    use ffmpeg_next::ffi::{AVColorPrimaries as P, AVColorSpace as M, AVColorTransferCharacteristic as T};
    match delivery {
        klipp_core::HdrMode::Sdr => match odt {
            klipp_core::OdtChoice::DciP3D65_48 => ColorTags {
                primaries: P::AVCOL_PRI_SMPTE432,
                transfer: T::AVCOL_TRC_UNSPECIFIED,
                matrix: M::AVCOL_SPC_BT709,
                x265: "colorprim=smpte432:transfer=unknown:colormatrix=bt709",
            },
            // SDR in BT.2020 (#901): its primaries, BT.2020's SDR
            // transfer (the BT.709 curve under BT.2020's name), and the
            // BT.2020 non-constant-luminance matrix the scalers then
            // convert with — see `yuv_matrix_for`.
            klipp_core::OdtChoice::Rec2020Sdr => ColorTags {
                primaries: P::AVCOL_PRI_BT2020,
                transfer: T::AVCOL_TRC_BT2020_10,
                matrix: M::AVCOL_SPC_BT2020_NCL,
                x265: "colorprim=bt2020:transfer=bt2020-10:colormatrix=bt2020nc",
            },
            _ => ColorTags {
                primaries: P::AVCOL_PRI_BT709,
                transfer: T::AVCOL_TRC_BT709,
                matrix: M::AVCOL_SPC_BT709,
                x265: "colorprim=bt709:transfer=bt709:colormatrix=bt709",
            },
        },
        klipp_core::HdrMode::Hlg => ColorTags {
            primaries: P::AVCOL_PRI_BT2020,
            transfer: T::AVCOL_TRC_ARIB_STD_B67,
            matrix: M::AVCOL_SPC_BT2020_NCL,
            x265: "colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
        },
        klipp_core::HdrMode::Pq => ColorTags {
            primaries: P::AVCOL_PRI_BT2020,
            transfer: T::AVCOL_TRC_SMPTE2084,
            matrix: M::AVCOL_SPC_BT2020_NCL,
            x265: "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
        },
    }
}

/// The Y'CbCr matrix an export converts with, the one [`color_tags`]
/// names: BT.2020 non-constant-luminance for HDR and for Rec.2020 SDR,
/// BT.709 for every other SDR delivery.
fn yuv_matrix_for(settings: &ExportSettings) -> klipp_core::ColorMatrix {
    match color_tags(settings.delivery(), settings.odt).matrix {
        ffmpeg_next::ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL => klipp_core::ColorMatrix::Bt2020Ncl,
        _ => klipp_core::ColorMatrix::Bt709,
    }
}

/// Write the colour tags from [`color_tags`] onto the encoder's
/// AVCodecContext.
/// Safety: the caller must hold a valid `&mut Encoder` context; `as_mut_ptr`
/// returns a non-null pointer for the lifetime of the context.
fn apply_color_tags(video_ctx: &mut ffmpeg_next::codec::encoder::video::Video, settings: &ExportSettings) {
    let tags = color_tags(settings.delivery(), settings.odt);
    unsafe {
        (*video_ctx.as_mut_ptr()).color_primaries = tags.primaries;
        (*video_ctx.as_mut_ptr()).color_trc = tags.transfer;
        (*video_ctx.as_mut_ptr()).colorspace = tags.matrix;
    }
}

/// One frame's light levels as they accumulate: the brightest pixel and
/// the sum over pixels, cd/m², each pixel by its brightest channel.
#[derive(Clone, Copy, Default)]
struct FrameLight {
    max: f32,
    sum: f64,
    pixels: u64,
}

impl FrameLight {
    fn add(self, nits: f32) -> Self {
        Self { max: self.max.max(nits), sum: self.sum + nits as f64, pixels: self.pixels + 1 }
    }
    fn merge(self, o: Self) -> Self {
        Self { max: self.max.max(o.max), sum: self.sum + o.sum, pixels: self.pixels + o.pixels }
    }
}

/// A PQ signal's light, cd/m².
fn pq_light(signal: f32) -> f32 {
    klipp_color::hdr::pq_decode_nits(signal)
}

/// An export's HDR10+ (#468), when the settings ask for it on an H.265 PQ
/// delivery: its shots start where a clip starts on a video track, in
/// export frames.
fn hdr10plus(settings: &ExportSettings, snapshot: &SequenceSnapshot) -> Option<crate::hdr10plus::Hdr10Plus> {
    if !(settings.hdr10plus && settings.renders_hdr() && settings.delivery() == klipp_core::HdrMode::Pq && matches!(settings.video_codec, ExportVideoCodec::H265)) {
        return None;
    }
    let (fps, start, end) = (snapshot.fps.max(1.0), snapshot.start_frame(), snapshot.end_frame());
    let frame = |t: &klipp_core::Timecode| (t.to_secs_f64() * fps).round() as u64;
    let starts = snapshot
        .video
        .sequence
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
        .flat_map(|t| t.clips.iter().map(|c| frame(&c.timeline_in)))
        .filter(|&f| f > start && f < end)
        .map(|f| f - start)
        .collect();
    Some(crate::hdr10plus::Hdr10Plus::new(starts))
}

/// The captions an export carries in its video stream (#1663): its first
/// subtitle track as CEA-608 CC1, timed from the export's first frame,
/// when the settings ask and the codec is H.264 or H.265.
fn captions_608(settings: &ExportSettings, snapshot: &SequenceSnapshot) -> Option<crate::a53::Captions> {
    let codec = match settings.video_codec {
        ExportVideoCodec::H264 => crate::a53::Codec::H264,
        ExportVideoCodec::H265 => crate::a53::Codec::Hevc,
        _ => return None,
    };
    let track = snapshot.subtitle_tracks.first().filter(|_| settings.captions_608)?;
    let (fps, start) = (snapshot.fps.max(1.0), snapshot.start_frame());
    let cues: Vec<klipp_captions::SubtitleEntry> = track
        .clips
        .iter()
        .filter(|c| c.timeline_end_frame > start)
        .map(|c| klipp_captions::SubtitleEntry {
            start_secs: c.timeline_start_frame.saturating_sub(start) as f64 / fps,
            end_secs: (c.timeline_end_frame - start) as f64 / fps,
            text: c.text.clone(),
            spans: Vec::new(),
            placement: Default::default(),
        })
        .collect();
    Some(crate::a53::Captions::new(klipp_captions::scc::cc1_pairs(&cues), fps, codec))
}

/// The per-pixel display transform an export with these settings writes.
///
/// An HDR render (`ExportSettings::renders_hdr`) is display light, encoded
/// straight to PQ or HLG. Otherwise the picture is SDR and goes through
/// the output transform: "Match viewer" is the viewer's, which applies no
/// display tone map (scene-referred clips are tone-mapped per clip, before
/// the composite); the operator picked in the export dialog applies to the
/// legacy sRGB path only, the ACES transforms carrying their own curves.
/// `working` is the project's working colorspace as the export starts.
pub fn export_display(settings: &ExportSettings, working: klipp_core::WorkingColorspace) -> klipp_color::display::ExportDisplay {
    use klipp_color::display::ExportDisplay;
    if settings.renders_hdr() {
        return ExportDisplay::hdr(working, settings.delivery(), settings.hdr_peak_nits());
    }
    let tone_map_op = match settings.odt {
        klipp_core::OdtChoice::MatchViewer | klipp_core::OdtChoice::Unknown => klipp_core::ToneMapOp::None,
        _ => settings.tone_map_op,
    };
    ExportDisplay::new(tone_map_op, working, settings.odt)
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
        // The container is the user's: `container_problem` refuses a
        // codec it cannot hold, so ProRes and DNxHD go in MKV as well as
        // MOV.
        if let Some(problem) = settings.container_problem() {
            return Err(RenderError::Encode(problem));
        }
        let forced_format = match settings.format {
            ExportFormat::Mp4 => "mp4",
            ExportFormat::Mkv => "matroska",
            ExportFormat::Mov => "mov",
            ExportFormat::WebM => "webm",
            ExportFormat::Gif => "gif",
            // Sound alone is written by `crate::audio_only` (#1813).
            ExportFormat::Mp3 | ExportFormat::Wav | ExportFormat::M4a => {
                return Err(RenderError::Encode(format!("{} has no picture; the audio export writes it", settings.format.label())));
            }
        };

        let mut output_ctx = ffmpeg_next::format::output_as(output_path, forced_format)
            .map_err(|e| {
                RenderError::Encode(format!("output context (forced {forced_format}): {e}"))
            })?;

        // ═══════════════════════════════════════════════════════════
        // VIDEO ENCODER
        // ═══════════════════════════════════════════════════════════

        // Hardware first: NVENC for NVIDIA, AMF for AMD, VAAPI for AMD and
        // Intel, then software (libx264/libx265). Which of them the GPU
        // actually has is only found out by opening them, and a consumer
        // card allows only a few NVENC sessions at once, so OBS, a call, a
        // proxy job or another export can hold the last one. A refusal
        // moves on to the next encoder rather than failing the render
        // (#381, #555).
        // The encoders are picked for the size they will be given.
        let (enc_w, enc_h) = settings.output_size(snapshot.width, snapshot.height);
        let attempts = video_encoder_attempts(
            &settings.video_codec,
            enc_w,
            enc_h,
            settings.bit_depth == 10,
            &crate::vaapi::encode_nodes(),
        );
        #[cfg(test)]
        let attempts = ATTEMPTS.with(|a| a.borrow().clone()).unwrap_or(attempts);
        let last_attempt = attempts.len() - 1;
        let mut hardware_fallback = None;
        // Why the earlier attempts failed, named in the error if the last
        // fails too, so it still says which hardware encoders were tried.
        let mut earlier_failure: Option<String> = None;
        let (video_encoder, video_codec_name, pix_fmt, out_w, out_h) = 'open: {
            for (attempt, EncoderAttempt { name: video_codec_name, hardware: is_hw_encoder, vaapi: vaapi_node }) in
                attempts.into_iter().enumerate()
            {
                    let Some(video_codec) = ffmpeg_next::encoder::find_by_name(video_codec_name) else {
                        // An FFmpeg built with nvenc but not libx264 lands
                        // here on the fallback; it fails like an open does.
                        let failure = format!("{video_codec_name}: codec not found in this FFmpeg");
                        if attempt == last_attempt {
                            return Err(RenderError::Encode(match earlier_failure.take() {
                                Some(earlier) => format!("open video encoder: {earlier}; then {failure}"),
                                None => format!("codec not found: {video_codec_name}"),
                            }));
                        }
                        tracing::warn!("{failure}; trying the next encoder");
                        hardware_fallback.get_or_insert(video_codec_name);
                        earlier_failure = Some(match earlier_failure.take() {
                            Some(earlier) => format!("{earlier}; then {failure}"),
                            None => failure,
                        });
                        continue;
                    };

                    info!("Video encoder: {} (hw={})", video_codec_name, is_hw_encoder);
                    // A 4K HDR H.264 render is much slower on libx264 than the user's
                    // last 8-bit one was on NVENC, so say why rather than leaving them
                    // to wonder. HEVC keeps the hardware path at ten bits.
                    if settings.bit_depth == 10 && matches!(settings.video_codec, ExportVideoCodec::H264) {
                        info!(
                            "10-bit H.264 encodes in software: no hardware H.264 encoder does ten bits. \
                             H.265 would encode on the GPU at this depth."
                        );
                    }

                    // Create encoder context
                    let mut video_ctx = ffmpeg_next::codec::context::Context::new_with_codec(video_codec)
                        .encoder()
                        .video()
                        .map_err(|e| RenderError::Encode(format!("video encoder context: {e}")))?;

                    // The size the export job renders at, even on both sides.
                    let (out_w, out_h) = settings.output_size(snapshot.width, snapshot.height);
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
                    // #65 baseline: for H.264/H.265/AV1, bit_depth == 10 selects a 10-bit
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
                        // AV1 was always 8-bit here, so a 10-bit or HDR AV1
                        // export carried its PQ or HLG in 8 bits (#560).
                        // Every AV1 encoder takes ten: NVENC, AMF and VAAPI
                        // as P010, SVT-AV1 planar.
                        ExportVideoCodec::Av1 => match (is_hw_encoder, want_10bit) {
                            (true, true) => ffmpeg_next::format::Pixel::P010LE,
                            (true, false) => ffmpeg_next::format::Pixel::NV12,
                            (false, true) => ffmpeg_next::format::Pixel::YUV420P10LE,
                            (false, false) => ffmpeg_next::format::Pixel::YUV420P,
                        },
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
                        ExportVideoCodec::H264 | ExportVideoCodec::H265 | ExportVideoCodec::Av1 if is_hw_encoder => {
                            hardware_quality(video_codec_name, settings, &mut video_ctx, &mut video_opts);
                            if let Some((_, true)) = vaapi_node {
                                video_opts.set("low_power", "1");
                            }
                            // Colour-space tagging (BT.709 SDR or BT.2020 HDR — #65).
                            apply_color_tags(&mut video_ctx, settings);
                        }
                        ExportVideoCodec::H264 => {
                            video_ctx.set_bit_rate(0); // CRF mode
                            video_opts.set("crf", &settings.crf.to_string());
                            video_opts.set("preset", settings.preset.ffmpeg_value());
                            apply_color_tags(&mut video_ctx, settings);
                        }
                        ExportVideoCodec::H265 => {
                            video_ctx.set_bit_rate(0);
                            video_opts.set("crf", &settings.crf.to_string());
                            video_opts.set("preset", settings.preset.ffmpeg_value());
                            video_opts.set("x265-params", color_tags(settings.delivery(), settings.odt).x265);
                            apply_color_tags(&mut video_ctx, settings);
                        }
                        ExportVideoCodec::ProRes422 => {
                            video_opts.set("profile", "2");
                        }
                        ExportVideoCodec::ProRes422HQ => {
                            video_opts.set("profile", "3");
                        }
                        ExportVideoCodec::ProRes4444 => {
                            video_opts.set("profile", "4");
                        }
                        ExportVideoCodec::DNxHD115 => {
                            video_ctx.set_bit_rate(115_000_000);
                        }
                        ExportVideoCodec::DNxHD145 => {
                            video_ctx.set_bit_rate(145_000_000);
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
                        apply_color_tags(&mut video_ctx, settings);
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
                                // AMF and VAAPI name their rate control.
                                if video_codec_name.ends_with("_vaapi") {
                                    video_opts.set("rc_mode", "CBR");
                                } else if video_codec_name.ends_with("_amf") {
                                    video_opts.set("rc", "cbr");
                                }
                            }
                        }
                        klipp_core::RateControl::Vbr => {
                            if let Some(br_kbps) = settings.video_bitrate_kbps {
                                video_ctx.set_bit_rate(br_kbps as usize * 1000);
                                video_opts.set("maxrate", &format!("{}k", br_kbps * 2));
                                video_opts.set("bufsize", &format!("{}k", br_kbps * 4));
                                if video_codec_name.ends_with("_vaapi") {
                                    video_opts.set("rc_mode", "VBR");
                                } else if video_codec_name.ends_with("_amf") {
                                    video_opts.set("rc", "vbr_peak");
                                }
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
                #[cfg(test)]
                let refused = if is_hw_encoder {
                    FAIL_HARDWARE_OPEN.with(|f| f.get())
                } else {
                    FAIL_SOFTWARE_OPEN.with(|f| f.get())
                };
                #[cfg(not(test))]
                let refused = false;
                // A VAAPI encoder takes surfaces rather than frames in
                // memory, so it is given a pool of them on its node first.
                let opened = if refused {
                    Err(ffmpeg_next::Error::External.to_string())
                } else if let Some((node, _)) = &vaapi_node {
                    crate::vaapi::attach(&mut video_ctx, node, pix_fmt, out_w, out_h).and_then(|()| {
                        video_ctx.open_as_with(video_codec, video_opts).map_err(|e| format!("{e} on {}", node.display()))
                    })
                } else {
                    video_ctx.open_as_with(video_codec, video_opts).map_err(|e| e.to_string())
                };
                match opened {
                    Ok(encoder) => {
                        // A later hardware encoder that opened is not a
                        // fallback to software.
                        if is_hw_encoder {
                            hardware_fallback = None;
                        }
                        break 'open (encoder, video_codec_name, pix_fmt, out_w, out_h);
                    }
                    Err(e) => {
                        // Naming the combination, because the library's own
                        // message for a configuration the hardware cannot do
                        // is "Generic error in an external library" and
                        // nothing else.
                        let failure = format!(
                            "{video_codec_name}: {e} ({pix_fmt:?}, {}x{}{})",
                            snapshot.width,
                            snapshot.height,
                            if settings.bit_depth == 10 { ", 10-bit" } else { "" },
                        );
                        if attempt == last_attempt {
                            return Err(RenderError::Encode(match earlier_failure.take() {
                                Some(earlier) => format!("open video encoder: {earlier}; then {failure}"),
                                None => format!("open video encoder: {failure}"),
                            }));
                        }
                        tracing::warn!("open video encoder: {failure}; trying the next encoder");
                        hardware_fallback.get_or_insert(video_codec_name);
                        earlier_failure = Some(match earlier_failure.take() {
                            Some(earlier) => format!("{earlier}; then {failure}"),
                            None => failure,
                        });
                    }
                }
            }
            unreachable!("the last attempt returns its error");
        };

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
        // A GIF's frames go through its palette filter graph, never this
        // scaler, and swscale makes no RGBA → PAL8 scaler: asked for one,
        // every GIF export failed at open (#1142). It gets a copy instead.
        let scaler_out = if settings.video_codec == ExportVideoCodec::Gif { ffmpeg_next::format::Pixel::RGBA } else { pix_fmt };
        let mut rgba_to_yuv = ffmpeg_next::software::scaling::Context::get(
            ffmpeg_next::format::Pixel::RGBA,
            scaler_in_w,
            scaler_in_h,
            scaler_out,
            out_w,
            out_h,
            ffmpeg_next::software::scaling::Flags::LANCZOS,
        )
        .map_err(|e| RenderError::Encode(format!("RGBA→YUV scaler: {e}")))?;
        // Write the matrix the tags declare (swscale would use BT.601),
        // limited range as tagged above.
        if settings.video_codec != ExportVideoCodec::Gif {
            let matrix = match settings.delivery() {
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
        // A GIF has no sound: its muxer refuses an audio stream, whatever
        // audio codec the settings carry (#1142).
        let skip_audio = matches!(settings.audio_codec, klipp_core::ExportAudioCodec::Copy) || settings.format == klipp_core::ExportFormat::Gif;
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

            // The codec's preferred sample format (not always f32), set
            // up as the sound-alone writer and the true-peak trials set
            // theirs (#1930).
            let sample_fmt = crate::audio_only::configure_audio_encoder(&mut audio_ctx, audio_codec, snapshot.sample_rate, settings.audio_bitrate_kbps);

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
                klipp_core::ExportFormat::Gif | klipp_core::ExportFormat::Mp3 | klipp_core::ExportFormat::Wav | klipp_core::ExportFormat::M4a => {
                    unreachable!("supports_subtitles guards this")
                }
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
            .write_header_with(crate::audio_only::header_options(settings.format))
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

        let audio_codec_name = audio_encoder.as_ref().map(|_| audio_codec_name);
        Ok(Self {
            output_ctx,
            video_encoder,
            audio_codec_name,
            audio_encoder,
            audio_bitrate_kbps: settings.audio_bitrate_kbps,
            video_stream_idx,
            audio_stream_idx,
            video_pts: 0,
            audio_pts: 0,
            video_time_base,
            audio_time_base,
            rgba_to_yuv,
            rgba64_to_yuv: None,
            yuv_matrix: yuv_matrix_for(settings),
            output_width: out_w,
            output_height: out_h,
            audio_frame_size,
            audio_buffer: Vec::new(),
            audio_resampler,
            audio_sample_format: audio_sample_fmt,
            audio_frame: match (audio_sample_fmt, audio_frame_size) {
                (Some(fmt), Some(size)) => Some(ffmpeg_next::frame::Audio::new(
                    fmt,
                    size,
                    ffmpeg_next::ChannelLayout::STEREO,
                )),
                _ => None,
            },
            video_codec_name,
            frame_format: pix_fmt,
            hardware_fallback,
            subtitle_stream_idx,
            hdr_mode: settings.delivery(),
            light_stats: LightLevelStats::new(),
            hdr_peak_nits: settings.hdr_peak_nits(),
            captions: captions_608(settings, snapshot),
            hdr10plus: hdr10plus(settings, snapshot),
            frame_tags: (settings.video_codec != ExportVideoCodec::Gif).then(|| color_tags(settings.delivery(), settings.odt)),
            display: export_display(settings, snapshot.working_colorspace),
            gif_filter: if matches!(settings.format, ExportFormat::Gif) {
                let fps_den = (snapshot.fps.max(1.0).round() as u32).max(1);
                Some(crate::gif_filter::GifFilter::new(out_w, out_h, fps_den)?)
            } else {
                None
            },
        })
    }

    /// The pixel format the encoder takes frames in (for VAAPI, the
    /// layout it uploads from).
    pub fn video_format(&self) -> ffmpeg_next::format::Pixel {
        self.frame_format
    }

    /// What its audio encoder opened as, if it has one (#1930).
    pub fn opened_audio(&self) -> Option<crate::audio_only::OpenedAudio> {
        self.audio_encoder.as_ref().map(|e| crate::audio_only::OpenedAudio::of(e, self.audio_bitrate_kbps))
    }

    /// The video encoder in use, as FFmpeg names it (`h264_nvenc`,
    /// `libx264`): what a status reports beside [`Self::hw_info`] (#1924).
    pub fn video_encoder_name(&self) -> &'static str {
        self.video_codec_name
    }

    /// The audio encoder in use, as FFmpeg names it; `None` without sound.
    pub fn audio_encoder_name(&self) -> Option<&'static str> {
        self.audio_codec_name
    }

    /// Human-readable hardware info for the video encoder.
    pub fn hw_info(&self) -> &'static str {
        if let Some(hw) = self.hardware_fallback {
            // Every machine's FFmpeg tries NVENC first, so it only means
            // something to name it on an NVIDIA one.
            return if hw.contains("nvenc") && klipp_core::GpuVendor::detect() == klipp_core::GpuVendor::Nvidia {
                "Software (CPU): NVENC unavailable"
            } else {
                "Software (CPU): hardware encoder unavailable"
            };
        }
        if self.video_codec_name.contains("nvenc") {
            "NVENC (NVIDIA)"
        } else if self.video_codec_name.contains("amf") {
            "AMF (AMD)"
        } else if self.video_codec_name.contains("vaapi") {
            "VAAPI (AMD/Intel)"
        } else if self.video_codec_name.contains("prores") {
            "ProRes (CPU)"
        } else if self.video_codec_name.contains("dnxhd") {
            "DNxHD (CPU)"
        } else {
            "Software (CPU)"
        }
    }

    /// Encode one rendered frame: linear RGBA f32, the picture as the
    /// renderer left it (display-referred for SDR, display light for an
    /// HDR render — see [`export_display`]).
    ///
    /// The export's display transform encodes it for the file: the
    /// viewer's sRGB encode, an ACES output transform, or for an HDR
    /// render PQ or HLG on BT.2020 primaries. A PQ delivery's light
    /// levels are measured on the signal written, for the MaxCLL /
    /// MaxFALL attached at [`Self::finalise`].
    pub fn encode_linear_frame(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
    ) -> Result<(), RenderError> {
        if self.gif_filter.is_none() {
            if let Some(light) = self.encode_linear_direct(linear_rgba, width, height)? {
                self.count_light(light);
                return Ok(());
            }
        }
        // 10-bit outputs take the transform at 16 bits — built from the
        // 8-bit result they would carry 8 bits of data.
        if self.is_deep() && self.gif_filter.is_none() {
            let display = self.display.frame(linear_rgba, |v, n| klipp_color::display::quantise(v, n, 65535.0) as u16);
            if self.hdr_mode == klipp_core::HdrMode::Pq {
                let light = display
                    .chunks_exact(4)
                    .map(|px| pq_light(px[0].max(px[1]).max(px[2]) as f32 / 65535.0))
                    .fold(FrameLight::default(), FrameLight::add);
                self.count_light(light);
            }
            return self.encode_video_frame_u16(&display, width, height);
        }
        let display_u8 = self.display.frame(linear_rgba, |v, n| klipp_color::display::quantise(v, n, 255.0) as u8);
        self.encode_video_frame(&display_u8, width, height)
    }

    /// What a GPU-produced frame would have to be for this encoder to take
    /// it: the tone map (0 none, 1 Reinhard, 2 Hable) and the output's
    /// luma coefficients.
    ///
    /// `None` when the frame has to come back as linear floats instead —
    /// a delivery whose display transform the shader does not have (an
    /// AcesCG-basis ODT, the ACES tone-map fit), a format that is not
    /// 4:2:0, or a GIF's palette filter.
    ///
    /// Also whether the chroma is interleaved, the sample depth, and the
    /// left shift each stored sample needs. NVENC takes NV12 at eight
    /// bits and P010 at ten; a software encoder takes the planar forms.
    /// P010 is the odd one: it carries its ten bits at the *top* of each
    /// u16, hence the shift.
    pub fn gpu_yuv420_params(&self) -> Option<klipp_effects::gpu::Yuv420Target> {
        use ffmpeg_next::format::Pixel as P;
        if self.gif_filter.is_some() {
            return None;
        }
        let (semi, bits, shift) = match self.frame_format {
            P::YUV420P => (false, 8, 0),
            P::NV12 => (true, 8, 0),
            P::YUV420P10LE => (false, 10, 0),
            P::P010LE => (true, 10, 6),
            _ => return None,
        };
        let separable = |op| match op {
            klipp_color::display::ToneMapOp::None => Some(0),
            klipp_color::display::ToneMapOp::Reinhard => Some(1),
            klipp_color::display::ToneMapOp::Hable => Some(2),
            // The ACES *tone-map fit* is not separable and is a
            // different thing from the ODT below; no shader twin.
            klipp_color::display::ToneMapOp::Aces => None,
        };
        // Which of the shader's four tails this delivery is. They are
        // mutually exclusive — each reads the same binding for its own
        // baked numbers — so this picks exactly one.
        //
        // `tone_map_op` 3 is the ACES Rec.709 output transform, which the
        // shader has in full. `gamut_op` 1 puts BT.2407's compression
        // into BT.709 in front of the tone map, which is what a BT.2020
        // project's SDR delivery is. `hdr_op` replaces the tone map and
        // the sRGB encode with BT.2020 and PQ or HLG. Otherwise a tone
        // map and an sRGB encode.
        let (op, odt_tag, gamut_op, hdr_op, peak_nits) = if let Some((pq, peak)) =
            self.display.hdr_transfer_and_peak()
        {
            (0, 0, 0, if pq { 1 } else { 2 }, peak)
        } else if self.display.is_aces_rec709() {
            (3, klipp_color::aces::GpuOdt::Rec709Sdr.tag(), 0, 0, 0.0)
        } else if let Some(odt) = self.display.acescg_basis_odt() {
            // The AcesCG-basis deliverables: the same `tone_map_op` as
            // the viewer match above, since the shader reads which
            // transform it is from the blob rather than the parameter.
            (3, odt.tag(), 0, 0, 0.0)
        } else if let Some(tm) = self.display.bt2020_gamut_then_tone_map_srgb() {
            (separable(tm)?, 0, 1, 0, 0.0)
        } else {
            (separable(self.display.tone_map_and_srgb_only()?)?, 0, 0, 0, 0.0)
        };
        let (kr, kb) = match self.yuv_matrix {
            klipp_core::ColorMatrix::Bt2020Ncl | klipp_core::ColorMatrix::Bt2020Cl => (0.2627f32, 0.0593f32),
            _ => (0.2126, 0.0722),
        };
        Some(klipp_effects::gpu::Yuv420Target {
            tone_map_op: op,
            odt_tag,
            // The renderer fills this in: only it knows whether the clip
            // on screen is scene-referred, and the encoder never sees a
            // source.
            scene: None,
            gamut_op,
            hdr_op,
            peak_nits,
            luma_coefs: [kr, 1.0 - kr - kb, kb],
            semi_planar: semi,
            bits,
            sample_shift: shift,
        })
    }

    /// Encode a frame the GPU already converted: 8-bit 4:2:0, luma then
    /// chroma, tightly packed, in whichever layout
    /// [`Self::gpu_yuv420_params`] asked for.
    ///
    /// Only the row copies into the encoder's frame happen here — the
    /// display transform and the colour conversion were done on the GPU,
    /// which is the point. The planes are tightly packed and an AVFrame's
    /// are not, so each row is copied to its own stride.
    pub fn encode_yuv420_planes(
        &mut self,
        planes: &[u8],
        width: u32,
        height: u32,
        light: Option<klipp_effects::gpu::GpuFrameLight>,
    ) -> Result<(), RenderError> {
        use ffmpeg_next::format::Pixel as P;
        use rayon::prelude::*;
        // A PQ delivery's MaxCLL and MaxFALL, measured by the shader that
        // wrote these planes rather than by a pass over a linear frame
        // that never came down.
        if let Some(l) = light {
            self.count_light(FrameLight {
                max: l.max_nits,
                sum: l.sum_nits,
                pixels: l.pixels,
            });
        }
        let fmt = self.frame_format;
        let Some(target) = self.gpu_yuv420_params() else {
            return Err(RenderError::Encode(format!("{fmt:?} takes no GPU planes")));
        };
        let want = target.byte_len(width, height);
        if planes.len() != want {
            return Err(RenderError::Encode(format!(
                "GPU planes are {} bytes, expected {want} for {width}x{height} at {} bits",
                planes.len(),
                target.bits,
            )));
        }
        let (wu, hu) = (width as usize, height as usize);
        // Row widths in *bytes*, which is what a stride is measured in.
        let sample = if target.bits == 8 { 1 } else { 2 };
        let luma = wu * hu * sample;

        let mut frame = ffmpeg_next::frame::Video::new(fmt, width, height);
        let (y_src, chroma) = planes.split_at(luma);
        // Y, then either one interleaved chroma plane of the same row
        // width or two planes of half.
        let mut rows: Vec<(usize, &[u8], usize, usize)> = vec![(0, y_src, wu * sample, hu)];
        match fmt {
            P::NV12 | P::P010LE => rows.push((1, chroma, wu * sample, hu / 2)),
            _ => {
                let (cb, cr) = chroma.split_at(chroma.len() / 2);
                rows.push((1, cb, (wu / 2) * sample, hu / 2));
                rows.push((2, cr, (wu / 2) * sample, hu / 2));
            }
        }
        for (plane, src, row_bytes, h) in rows {
            let stride = frame.stride(plane);
            frame.data_mut(plane)[..stride * h]
                .par_chunks_mut(stride)
                .zip(src.par_chunks(row_bytes))
                .for_each(|(dst, s)| dst[..row_bytes].copy_from_slice(s));
        }
        frame.set_pts(Some(self.video_pts));
        self.video_pts += 1;
        self.send_video_frame(&mut frame)?;
        self.drain_video_packets()
    }

    /// Send one picture to the video encoder, by way of a VAAPI surface
    /// when the encoder takes those.
    fn send_video_frame(&mut self, frame: &mut ffmpeg_next::frame::Video) -> Result<(), RenderError> {
        // Its colour as the stream is tagged. ProRes writes each frame's
        // own into its frame header, and a frame without them told every
        // decoder "unspecified", which they read as BT.601: saturated
        // colours shifted in every ProRes export (#1142).
        if let Some(t) = self.frame_tags {
            frame.set_color_primaries(t.primaries.into());
            frame.set_color_transfer_characteristic(t.transfer.into());
            frame.set_color_space(t.matrix.into());
            frame.set_color_range(ffmpeg_next::color::Range::MPEG);
        }
        // Its light for HDR10+, from every fourth pixel of the planes the
        // encoder is given (#468).
        if let Some(h) = self.hdr10plus.as_mut() {
            h.measured(crate::hdr10plus::measure(frame, 4));
        }
        let sent = if crate::vaapi::is_attached(&self.video_encoder) {
            let surface = crate::vaapi::upload(&self.video_encoder, frame).map_err(RenderError::Encode)?;
            self.video_encoder.send_frame(&surface)
        } else {
            self.video_encoder.send_frame(frame)
        };
        sent.map_err(|e| RenderError::Encode(format!("send video frame: {e}")))
    }

    /// Record a PQ frame's light levels (other deliveries signal none).
    fn count_light(&mut self, light: FrameLight) {
        if self.hdr_mode == klipp_core::HdrMode::Pq && light.pixels > 0 {
            self.light_stats.add_frame(light.max, (light.sum / light.pixels as f64) as f32);
        }
    }

    /// Linear frame → the output's Y'CbCr planes in one parallel pass: the
    /// export display transform per pixel, then the file's matrix, limited
    /// range, chroma averaged over each 2×2 (4:2:0) or 2×1 (4:2:2) block.
    /// Replaces display RGBA → single-threaded swscale, which cost 30+ ms a
    /// 4K frame, and skips the intermediate 8/16-bit rounding. Returns
    /// `false` for a pixel format it does not write, or odd dimensions;
    /// the caller then takes the swscale path. For a PQ delivery, also
    /// the frame's light levels.
    fn encode_linear_direct(&mut self, linear: &[f32], w: u32, h: u32) -> Result<Option<FrameLight>, RenderError> {
        use ffmpeg_next::format::Pixel as P;
        use rayon::prelude::*;
        let fmt = self.frame_format;
        let (bits, semi, sw, sh) = match fmt {
            P::YUV420P => (8u32, false, 1usize, 1usize),
            P::YUV422P => (8, false, 1, 0),
            P::NV12 => (8, true, 1, 1),
            P::YUV420P10LE => (10, false, 1, 1),
            P::YUV422P10LE => (10, false, 1, 0),
            P::YUV444P10LE => (10, false, 0, 0),
            P::P010LE => (10, true, 1, 1),
            _ => return Ok(None),
        };
        let (wu, hu) = (w as usize, h as usize);
        if (w, h) != (self.output_width, self.output_height)
            || linear.len() != wu * hu * 4
            || wu % (1 << sw) != 0
            || hu % (1 << sh) != 0
        {
            return Ok(None);
        }
        let display = self.display;
        let measure = self.hdr_mode == klipp_core::HdrMode::Pq;
        let (kr, kb) = match self.yuv_matrix {
            klipp_core::ColorMatrix::Bt2020Ncl | klipp_core::ColorMatrix::Bt2020Cl => (0.2627f32, 0.0593f32),
            _ => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let scale = (1u32 << (bits - 8)) as f32;
        let max = ((1u32 << bits) - 1) as f32;
        let bps = if bits > 8 { 2usize } else { 1 };
        let shift = if fmt == P::P010LE { 6 } else { 0 };
        let quant = move |v: f32, off: f32, range: f32| ((off + v * range) * scale + 0.5).clamp(0.0, max) as u16;

        let mut frame = ffmpeg_next::frame::Video::new(fmt, w, h);
        let (ys, cs) = (frame.stride(0), frame.stride(1));
        let vs = if semi { 0 } else { frame.stride(2) };
        // Raw plane addresses: every band below writes rows no other band
        // touches, and the frame outlives the parallel loop.
        let py = frame.data_mut(0).as_mut_ptr() as usize;
        let pu = frame.data_mut(1).as_mut_ptr() as usize;
        let pv = if semi { 0 } else { frame.data_mut(2).as_mut_ptr() as usize };
        let (rows, cols) = (1usize << sh, 1usize << sw);
        let cw = wu >> sw;

        let light = (0..hu >> sh).into_par_iter().map(|cy| unsafe {
            let mut light = FrameLight::default();
            let put = |base: usize, idx: usize, v: u16| {
                if bps == 2 {
                    let b = (v << shift).to_le_bytes();
                    *((base + idx * 2) as *mut u8) = b[0];
                    *((base + idx * 2 + 1) as *mut u8) = b[1];
                } else {
                    *((base + idx) as *mut u8) = v as u8;
                }
            };
            let mut rgb = vec![[0.0f32; 3]; wu * rows];
            for r in 0..rows {
                let y = cy * rows + r;
                let row = py + y * ys;
                for x in 0..wu {
                    let i = (y * wu + x) * 4;
                    let e = if measure {
                        let (e, nits) = display.encode_light([linear[i], linear[i + 1], linear[i + 2]]);
                        light = light.add(nits);
                        e
                    } else {
                        display.encode([linear[i], linear[i + 1], linear[i + 2]])
                    };
                    rgb[r * wu + x] = e;
                    put(row, x, quant(kr * e[0] + kg * e[1] + kb * e[2], 16.0, 219.0));
                }
            }
            let n = (rows * cols) as f32;
            for cx in 0..cw {
                let mut acc = [0.0f32; 3];
                for r in 0..rows {
                    for c in 0..cols {
                        let e = rgb[r * wu + cx * cols + c];
                        acc[0] += e[0];
                        acc[1] += e[1];
                        acc[2] += e[2];
                    }
                }
                let (red, grn, blu) = (acc[0] / n, acc[1] / n, acc[2] / n);
                let luma = kr * red + kg * grn + kb * blu;
                let cb = quant((blu - luma) / (2.0 * (1.0 - kb)), 128.0, 224.0);
                let cr = quant((red - luma) / (2.0 * (1.0 - kr)), 128.0, 224.0);
                if semi {
                    let row = pu + cy * cs;
                    put(row, cx * 2, cb);
                    put(row, cx * 2 + 1, cr);
                } else {
                    put(pu + cy * cs, cx, cb);
                    put(pv + cy * vs, cx, cr);
                }
            }
            light
        })
        .reduce(FrameLight::default, FrameLight::merge);

        frame.set_pts(Some(self.video_pts));
        self.video_pts += 1;
        self.send_video_frame(&mut frame)?;
        self.drain_video_packets()?;
        Ok(Some(light))
    }

    /// Whether the output format holds more than 8 bits per sample.
    pub fn is_deep(&self) -> bool {
        use ffmpeg_next::format::Pixel as P;
        matches!(self.frame_format, P::P010LE | P::YUV420P10LE | P::YUV422P10LE | P::YUV444P10LE)
    }

    /// Encode one 16-bit display-encoded RGBA frame (`width * height * 4`
    /// samples, full range) into a 10-bit output.
    pub fn encode_video_frame_u16(&mut self, rgba: &[u16], width: u32, height: u32) -> Result<(), RenderError> {
        use ffmpeg_next::format::Pixel;
        if self.rgba64_to_yuv.is_none() {
            let mut sws = ffmpeg_next::software::scaling::Context::get(
                Pixel::RGBA64LE,
                width,
                height,
                self.frame_format,
                self.output_width,
                self.output_height,
                ffmpeg_next::software::scaling::Flags::LANCZOS,
            )
            .map_err(|e| RenderError::Encode(format!("RGBA64→YUV scaler: {e}")))?;
            unsafe {
                crate::sws_colour::set_rgb_to_yuv(sws.as_mut_ptr() as *mut _, self.yuv_matrix, klipp_core::ColorRange::Limited);
            }
            self.rgba64_to_yuv = Some(sws);
        }
        let mut frame = ffmpeg_next::frame::Video::new(Pixel::RGBA64LE, width, height);
        let stride = frame.stride(0);
        let row = width as usize * 4;
        {
            let dst = frame.data_mut(0);
            for y in 0..height as usize {
                let d = &mut dst[y * stride..y * stride + row * 2];
                for (k, v) in rgba[y * row..(y + 1) * row].iter().enumerate() {
                    d[k * 2..k * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        let mut yuv = ffmpeg_next::frame::Video::new(self.frame_format, self.output_width, self.output_height);
        self.rgba64_to_yuv
            .as_mut()
            .unwrap()
            .run(&frame, &mut yuv)
            .map_err(|e| RenderError::Encode(format!("RGBA64→YUV scale: {e}")))?;
        yuv.set_pts(Some(self.video_pts));
        self.video_pts += 1;
        self.send_video_frame(&mut yuv)?;
        self.drain_video_packets()?;
        Ok(())
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
            self.frame_format,
            self.output_width,
            self.output_height,
        );
        self.rgba_to_yuv
            .run(&rgba_frame, &mut yuv_frame)
            .map_err(|e| RenderError::Encode(format!("RGBA→YUV scale: {e}")))?;

        yuv_frame.set_pts(Some(self.video_pts));
        self.video_pts += 1;

        // Send to encoder
        self.send_video_frame(&mut yuv_frame)?;

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
                if let Some(resampler) = self.audio_resampler.as_mut() {
                    // Another format than float (24-bit PCM is 32-bit
                    // integers): converted, not its bytes copied (#2783).
                    convert_into(resampler, &self.audio_buffer[..floats_per_frame], frame_size, frame)?;
                } else if is_planar {
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
        self.release_held_packets()?;

        // HDR10 signalling side-data has to land on the video
        // stream's codecpar before write_trailer: for MP4/MOV the
        // muxer writes the `clli` and `mdcv` atoms inside the moov
        // box at trailer time, reading from codecpar->coded_side_data.
        // Attach now, once the per-frame stats are final.
        // HDR10's metadata describes PQ; HLG is scene-relative and
        // carries none.
        if self.hdr_mode == klipp_core::HdrMode::Pq {
            if let Err(e) = self.attach_hdr10_side_data() {
                warn!(
                    "HDR10 side-data attach failed ({e}); file will be tagged \
                     BT.2020/PQ but MaxCLL/MaxFALL/mastering display missing",
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
                    _ if self.audio_resampler.is_some() => {
                        if let Some(resampler) = self.audio_resampler.as_mut() {
                            convert_into(resampler, &self.audio_buffer, frame_size, &mut frame)?;
                        }
                    }
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
            // An encoder that reorders (x264's B-frames) hands packets back
            // with no duration. The MP4 muxer then takes the last frame to
            // last no time and ends the edit list where it starts, so a
            // player drops it (#557). The time base is one frame a tick.
            if packet.duration() == 0 {
                packet.set_duration(1);
            }
            // With HDR10+ a packet waits for its shot's light (#468).
            match self.hdr10plus.as_mut() {
                Some(h) => {
                    h.hold(std::mem::replace(&mut packet, ffmpeg_next::Packet::empty()));
                    for ready in h.ready(false) {
                        self.write_video_packet(ready)?;
                    }
                }
                None => {
                    let p = std::mem::replace(&mut packet, ffmpeg_next::Packet::empty());
                    self.write_video_packet(p)?;
                }
            }
        }
        Ok(())
    }

    /// Every packet still waiting for its shot's HDR10+, the encoder
    /// flushed (#468).
    fn release_held_packets(&mut self) -> Result<(), RenderError> {
        let ready = self.hdr10plus.as_mut().map(|h| h.ready(true)).unwrap_or_default();
        for p in ready {
            self.write_video_packet(p)?;
        }
        Ok(())
    }

    /// Write one encoded picture: its captions put in (#1663), its times
    /// in the stream's time base.
    fn write_video_packet(&mut self, mut packet: ffmpeg_next::Packet) -> Result<(), RenderError> {
        // Its picture's captions, by its presentation time: a frame
        // a tick (#1663).
        if let (Some(captions), Some(pts), Some(data)) = (&self.captions, packet.pts(), packet.data()) {
            let mut with = ffmpeg_next::Packet::copy(&captions.insert(data, pts.max(0) as u64));
            with.set_stream(self.video_stream_idx);
            with.set_pts(packet.pts());
            with.set_dts(packet.dts());
            with.set_duration(packet.duration());
            with.set_flags(packet.flags());
            packet = with;
        }
        packet.rescale_ts(
            self.video_time_base,
            self.output_ctx
                .stream(self.video_stream_idx)
                .unwrap()
                .time_base(),
        );
        packet
            .write_interleaved(&mut self.output_ctx)
            .map_err(|e| RenderError::Encode(format!("write video packet: {e}")))
    }

    /// Attach HDR10 signalling side-data (Content Light Level +
    /// Mastering Display Colour Volume) to the video stream.
    ///
    /// Called once, at finalise time, for a PQ delivery. For MP4/
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

        use klipp_color::hdr::{bt2020_primaries, DEFAULT_MASTERING_MIN_LUMA_CD_M2};

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
            max_luminance: lum_rat(self.hdr_peak_nits),
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


/// `samples` (packed stereo floats, `frames` of them) into `out`, an
/// encoder frame of another format, through `resampler` (#2783).
fn convert_into(resampler: &mut ffmpeg_next::software::resampling::Context, samples: &[f32], frames: usize, out: &mut ffmpeg_next::frame::Audio) -> Result<(), RenderError> {
    let mut src = ffmpeg_next::frame::Audio::new(ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed), frames, ffmpeg_next::ChannelLayout::STEREO);
    src.set_rate(resampler.input().rate);
    let bytes: &[u8] = bytemuck::cast_slice(samples);
    let dst = src.data_mut(0);
    let n = bytes.len().min(dst.len());
    dst[..n].copy_from_slice(&bytes[..n]);
    resampler.run(&src, out).map_err(|e| RenderError::Encode(format!("audio conversion: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {

    /// A 10-bit H.264 export never goes to a hardware encoder.
    ///
    /// No hardware H.264 encoder does ten bits — NVENC's and AMF's H.264
    /// blocks are 8-bit 4:2:0 — and asking one to anyway fails inside
    /// `avcodec_open2` with "No capable devices found", which reaches the
    /// user as "Generic error in an external library" on a render that
    /// looked perfectly reasonable. Since the export UI only offers HDR at
    /// ten bits, that was every HDR H.264 render.
    ///
    /// Asserted as "not hardware" rather than "is libx264", so the test
    /// says the same thing on a machine with no NVENC at all.
    #[test]
    fn ten_bit_h264_never_uses_a_hardware_encoder() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let (name, hw) = pick_video_encoder(&ExportVideoCodec::H264, 3840, 2160, true);
        assert!(!hw, "10-bit H.264 picked the hardware encoder {name}, which cannot do it");
        // And 8-bit is left alone: the point is not to give up hardware
        // H.264, only to stop asking it for what it cannot do. Guarded on
        // the encoder existing, so this says something real on a machine
        // with NVENC and nothing false on one without.
        if ffmpeg_next::encoder::find_by_name("h264_nvenc").is_some() {
            let (name8, hw8) = pick_video_encoder(&ExportVideoCodec::H264, 3840, 2160, false);
            assert!(hw8, "8-bit H.264 stopped using hardware; it picked {name8}");
        }

        // HEVC and AV1 hardware *do* ten bits, so they keep it.
        for codec in [ExportVideoCodec::H265, ExportVideoCodec::Av1] {
            let (a, _) = pick_video_encoder(&codec, 3840, 2160, true);
            let (b, _) = pick_video_encoder(&codec, 3840, 2160, false);
            assert_eq!(a, b, "{codec:?} changed encoder for 10-bit, but its hardware can do it");
        }
    }
    use super::*;
    use crate::snapshot::SequenceSnapshot;
    use klipp_core::{
        EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec,
    };
    use serial_test::serial;

    fn make_settings(codec: ExportVideoCodec, path: &str) -> ExportSettings {
        ExportSettings {
            pan_law: Default::default(),
            output_path: std::path::PathBuf::from(path),
            format: ExportFormat::Mp4,
            video_codec: codec,
            resolution: None,
            fps: None,
            crf: 23,
            preset: EncoderPreset::Fast,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(128), video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
            bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
            odt: klipp_core::OdtChoice::MatchViewer,
            mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS }
    }

    fn make_snapshot() -> SequenceSnapshot {
        SequenceSnapshot {
            audio_buses: Vec::new(),
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: 320,
            height: 240,
            sample_rate: 48000,
            duration_frames: 10,
            export_range: None,
            video: Default::default(),
            audio_tracks: vec![],
            pan_law: Default::default(),
            global_lut_path: None,
            subtitle_tracks: vec![], chapters: vec![],
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
        }
    }

    fn fallback_settings(name: &str) -> (std::path::PathBuf, ExportSettings, SequenceSnapshot) {
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        let mut settings = make_settings(ExportVideoCodec::H264, path.to_str().unwrap());
        settings.audio_codec = ExportAudioCodec::Copy;
        (path, settings, make_snapshot())
    }

    /// #381: a hardware encoder that will not open (every NVENC session
    /// taken) must not fail the render; it goes to software and says so.
    #[test]
    #[serial]
    fn a_hardware_encoder_that_will_not_open_falls_back_to_software() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let (path, settings, snapshot) = fallback_settings("test_hw_fallback.mp4");
        if !pick_video_encoder(&ExportVideoCodec::H264, snapshot.width, snapshot.height, false).1 {
            eprintln!("skipped: this FFmpeg has no hardware H.264 encoder, so there is nothing to fall back from");
            return;
        }
        FAIL_HARDWARE_OPEN.with(|f| f.set(true));
        let opened = Encoder::open(&path, &settings, &snapshot);
        FAIL_HARDWARE_OPEN.with(|f| f.set(false));
        let mut enc = opened.expect("the render must fall back to software, not fail");

        assert_eq!(enc.video_codec_name, "libx264");
        assert!(enc.hardware_fallback.is_some(), "the fallback must be recorded");
        assert!(enc.hw_info().contains("unavailable"), "{}", enc.hw_info());
        let rgba = [0u8, 128, 255, 255].repeat(320 * 240);
        for _ in 0..5 {
            enc.encode_video_frame(&rgba, 320, 240).unwrap();
        }
        enc.finalise().unwrap();
        match std::process::Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,nb_read_frames", "-of", "csv=p=0"])
            .arg(&path)
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                assert!(text.contains("h264"), "the file is still an H.264 video: {text}");
                assert!(text.trim().ends_with(",5"), "all five frames are in the file: {text}");
            }
            Err(e) => eprintln!("ffprobe not available ({e}); the file was written but not probed"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// When the software encoder fails too, the error still says the
    /// hardware encoder was tried first, and why it was not used.
    #[test]
    #[serial]
    fn when_both_encoders_fail_the_error_names_both() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let (path, settings, snapshot) = fallback_settings("test_hw_both_fail.mp4");
        let (hw, had_hardware) = pick_video_encoder(&ExportVideoCodec::H264, snapshot.width, snapshot.height, false);
        if !had_hardware {
            eprintln!("skipped: this FFmpeg has no hardware H.264 encoder");
            return;
        }
        FAIL_HARDWARE_OPEN.with(|f| f.set(true));
        FAIL_SOFTWARE_OPEN.with(|f| f.set(true));
        let opened = Encoder::open(&path, &settings, &snapshot);
        FAIL_HARDWARE_OPEN.with(|f| f.set(false));
        FAIL_SOFTWARE_OPEN.with(|f| f.set(false));
        let Err(RenderError::Encode(message)) = opened else { panic!("both failing must fail the render") };
        assert!(message.contains(hw) && message.contains("then libx264"), "{message}");
        let _ = std::fs::remove_file(&path);
    }

    /// #557: a player shows every frame the render wrote. The encoder
    /// reorders frames (x264's B-frames start the stream at a negative
    /// dts), so the MP4 needs an edit list, and that list ended a frame
    /// early: the last frame was in the file but marked to be discarded.
    #[test]
    #[serial]
    fn every_frame_rendered_is_played() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let (path, settings, snapshot) = fallback_settings("test_last_frame.mp4");
        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();
        for colour in [[0u8, 128, 255, 255], [255, 64, 0, 255]] {
            let rgba = colour.repeat(320 * 240);
            for _ in 0..5 {
                enc.encode_video_frame(&rgba, 320, 240).unwrap();
            }
        }
        enc.finalise().unwrap();
        match std::process::Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
            .arg(&path)
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                assert_eq!(text.trim(), "10", "a player shows every frame rendered");
            }
            Err(e) => eprintln!("ffprobe not available ({e}); the file was written but not probed"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// #555: every hardware encoder FFmpeg has is tried before software,
    /// NVENC then AMF then VAAPI, and VAAPI on each AMD or Intel node, in
    /// low power too. Before, only the first one FFmpeg had was tried, so
    /// an AMD or Intel machine (whose FFmpeg has NVENC too) went straight
    /// to software.
    #[test]
    fn every_hardware_encoder_is_tried_before_software() {
        let _watch = crate::test_watchdog::Watchdog::start();
        ffmpeg_next::init().unwrap();
        let nodes = [std::path::PathBuf::from("/dev/dri/renderD128"), std::path::PathBuf::from("/dev/dri/renderD129")];
        let attempts = video_encoder_attempts(&ExportVideoCodec::H265, 1920, 1080, true, &nodes);
        let software = attempts.last().unwrap();
        assert_eq!((software.name, software.hardware, &software.vaapi), ("libx265", false, &None));
        let have = |name| ffmpeg_next::encoder::find_by_name(name).is_some();
        let mut expected: Vec<(&str, Option<(&str, bool)>)> = Vec::new();
        for name in ["hevc_nvenc", "hevc_amf"].into_iter().filter(|n| have(n)) {
            expected.push((name, None));
        }
        if have("hevc_vaapi") {
            for node in ["/dev/dri/renderD128", "/dev/dri/renderD129"] {
                expected.push(("hevc_vaapi", Some((node, false))));
                expected.push(("hevc_vaapi", Some((node, true))));
            }
        }
        let got: Vec<(&str, Option<(&str, bool)>)> = attempts[..attempts.len() - 1]
            .iter()
            .map(|a| {
                assert!(a.hardware, "{} is listed before software", a.name);
                (a.name, a.vaapi.as_ref().map(|(node, low)| (node.to_str().unwrap(), *low)))
            })
            .collect();
        assert_eq!(got, expected);

        // No VAAPI node, no VAAPI attempt; too small or 10-bit H.264, no
        // hardware at all.
        let none = video_encoder_attempts(&ExportVideoCodec::H264, 1920, 1080, false, &[]);
        assert!(none.iter().all(|a| a.vaapi.is_none()));
        for (w, h, deep) in [(64, 64, false), (1920, 1080, true)] {
            let only = video_encoder_attempts(&ExportVideoCodec::H264, w, h, deep, &nodes);
            assert_eq!(only.iter().map(|a| a.name).collect::<Vec<_>>(), ["libx264"], "{w}x{h} 10-bit={deep}");
        }
    }

    /// #555: AMF was given NVENC's options, `rc vbr` and `preset p5`,
    /// which it refuses while parsing them — so on AMD hardware it never
    /// opened, and every render went to software. Its own options parse:
    /// here, with no AMD runtime, the open gets as far as looking for the
    /// hardware, and on an AMD machine it opens.
    #[test]
    fn amf_is_given_options_it_accepts() {
        let _watch = crate::test_watchdog::Watchdog::start();
        ffmpeg_next::init().unwrap();
        for (codec, name) in [
            (ExportVideoCodec::H264, "h264_amf"),
            (ExportVideoCodec::H265, "hevc_amf"),
            (ExportVideoCodec::Av1, "av1_amf"),
        ] {
            let Some(amf) = ffmpeg_next::encoder::find_by_name(name) else {
                eprintln!("skipped {name}: not in this FFmpeg");
                continue;
            };
            let settings = make_settings(codec, "/dev/null");
            let mut ctx = ffmpeg_next::codec::context::Context::new_with_codec(amf).encoder().video().unwrap();
            ctx.set_width(320);
            ctx.set_height(240);
            ctx.set_time_base(ffmpeg_next::Rational::new(1, 24));
            ctx.set_format(ffmpeg_next::format::Pixel::NV12);
            let mut opts = ffmpeg_next::Dictionary::new();
            hardware_quality(name, &settings, &mut ctx, &mut opts);
            if let Err(e) = ctx.open_as_with(amf, opts) {
                assert_ne!(
                    e,
                    ffmpeg_next::Error::Other { errno: ffmpeg_next::util::error::EINVAL },
                    "{name} refused its options"
                );
            }
        }
    }

    /// #555: a render tries VAAPI and, where it cannot encode, goes on to
    /// software. On an AMD or Intel machine the VAAPI encoder opens and
    /// writes the file; on an NVIDIA one its VAAPI driver makes the
    /// surfaces but cannot encode, so this exercises the real fallback.
    #[test]
    #[serial]
    fn a_vaapi_render_encodes_or_falls_back_to_software() {
        let _watch = crate::test_watchdog::Watchdog::start();
        ffmpeg_next::init().unwrap();
        let Some((node, _)) = klipp_core::GpuVendor::render_nodes().into_iter().next() else {
            eprintln!("skipped: no render node");
            return;
        };
        if ffmpeg_next::encoder::find_by_name("h264_vaapi").is_none() {
            eprintln!("skipped: this FFmpeg has no VAAPI encoder");
            return;
        }
        let (path, settings, snapshot) = fallback_settings("test_vaapi.mp4");
        // Surfaces are at least 128 on a side for every hardware encoder.
        let snapshot = SequenceSnapshot { width: 320, height: 240, ..snapshot };
        ATTEMPTS.with(|a| {
            *a.borrow_mut() = Some(vec![
                EncoderAttempt { name: "h264_vaapi", hardware: true, vaapi: Some((node.clone(), false)) },
                EncoderAttempt { name: "libx264", hardware: false, vaapi: None },
            ])
        });
        let opened = Encoder::open(&path, &settings, &snapshot);
        ATTEMPTS.with(|a| *a.borrow_mut() = None);
        let mut enc = opened.expect("VAAPI encodes or the render falls back; it does not fail");
        match enc.video_codec_name {
            "h264_vaapi" => {
                assert_eq!(enc.hardware_fallback, None);
                assert_eq!(enc.hw_info(), "VAAPI (AMD/Intel)");
            }
            "libx264" => assert_eq!(enc.hardware_fallback, Some("h264_vaapi")),
            other => panic!("opened {other}"),
        }
        // The picture goes in as NV12 or planar YUV, whichever the encoder
        // took, and the frame paths write that layout.
        assert!(matches!(enc.video_format(), ffmpeg_next::format::Pixel::NV12 | ffmpeg_next::format::Pixel::YUV420P));
        let rgba = [0u8, 128, 255, 255].repeat(320 * 240);
        for _ in 0..5 {
            enc.encode_video_frame(&rgba, 320, 240).unwrap();
        }
        let linear = [0.2f32, 0.4, 0.8, 1.0].repeat(320 * 240);
        for _ in 0..5 {
            enc.encode_linear_frame(&linear, 320, 240).unwrap();
        }
        enc.finalise().unwrap();
        match std::process::Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,nb_read_frames", "-of", "csv=p=0"])
            .arg(&path)
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                assert!(text.contains("h264"), "an H.264 video: {text}");
                assert!(text.trim().ends_with(",10"), "all ten frames play: {text}");
            }
            Err(e) => eprintln!("ffprobe not available ({e}); the file was written but not probed"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// #560: a 10-bit AV1 export is ten bits in the file, through the
    /// hardware encoder and through SVT-AV1 alike. Both used to be given
    /// 8-bit pictures whatever the depth asked for. Delivered as HDR10,
    /// since the export dialog now offers AV1 that: the file says PQ on
    /// BT.2020.
    #[test]
    #[serial]
    fn ten_bit_av1_is_ten_bits_in_the_file() {
        let _watch = crate::test_watchdog::Watchdog::start();
        ffmpeg_next::init().unwrap();
        let mut candidates = vec![EncoderAttempt { name: "libsvtav1", hardware: false, vaapi: None }];
        if ffmpeg_next::encoder::find_by_name("av1_nvenc").is_some() {
            candidates.push(EncoderAttempt { name: "av1_nvenc", hardware: true, vaapi: None });
        }
        for attempt in candidates {
            let name = attempt.name;
            if ffmpeg_next::encoder::find_by_name(name).is_none() {
                eprintln!("skipped {name}: not in this FFmpeg");
                continue;
            }
            let (path, mut settings, snapshot) = fallback_settings(&format!("test_av1_10bit_{name}.mkv"));
            settings.video_codec = ExportVideoCodec::Av1;
            settings.format = ExportFormat::Mkv;
            settings.bit_depth = 10;
            settings.hdr_mode = klipp_core::HdrMode::Pq;
            ATTEMPTS.with(|a| *a.borrow_mut() = Some(vec![attempt]));
            let opened = Encoder::open(&path, &settings, &snapshot);
            ATTEMPTS.with(|a| *a.borrow_mut() = None);
            let mut enc = match opened {
                Ok(enc) => enc,
                // A GPU without AV1 encoding (before Ada, or none at all).
                Err(e) if name != "libsvtav1" => {
                    eprintln!("skipped {name}: {e}");
                    continue;
                }
                Err(e) => panic!("{name}: {e}"),
            };
            assert!(enc.is_deep(), "{name} takes 8-bit pictures for a 10-bit export: {:?}", enc.video_format());
            let linear = [0.2f32, 0.4, 0.8, 1.0].repeat(320 * 240);
            for _ in 0..5 {
                enc.encode_linear_frame(&linear, 320, 240).unwrap();
            }
            enc.finalise().unwrap();
            match std::process::Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,pix_fmt,color_transfer,color_primaries", "-of", "default=nw=1"])
                .arg(&path)
                .output()
            {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    for field in ["codec_name=av1", "pix_fmt=yuv420p10le", "color_transfer=smpte2084", "color_primaries=bt2020"] {
                        assert!(text.lines().any(|l| l == field), "{name}: no {field} in\n{text}");
                    }
                }
                Err(e) => eprintln!("ffprobe not available ({e}); the file was written but not probed"),
            }
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    #[serial]
    fn encode_h264_10_frames() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_h264.mp4");

        let mut settings = make_settings(ExportVideoCodec::H264, path.to_str().unwrap());
        // Skip audio in this test — audio encoding tested separately
        settings.audio_codec = ExportAudioCodec::Copy;
        let snapshot = make_snapshot();

        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();

        // Encode 10 red frames
        let rgba = [255u8, 0, 0, 255].repeat(320 * 240);
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
        let _watch = crate::test_watchdog::Watchdog::start();
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_h264_aac.mp4");

        let settings = make_settings(ExportVideoCodec::H264, path.to_str().unwrap());
        let snapshot = make_snapshot();

        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();

        let rgba = [128u8, 128, 128, 255].repeat(320 * 240);
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

    /// The tags follow the output transform applied, not only the
    /// delivery: a Rec.2020 SDR export says BT.2020 primaries, transfer
    /// and matrix (#901), a DCI-P3 D65 export says P3 primaries and claims no
    /// transfer (2.6 gamma has no code). The sRGB-curve exports, the sRGB
    /// transform and Match Viewer in a Rec.709 project, are tagged alike,
    /// as are Match Viewer and Rec.709 in ACEScg. H.265
    /// is checked as well as H.264 because libx265 takes its tags from
    /// x265-params. Skips without ffprobe.
    #[test]
    #[serial]
    fn sdr_tags_follow_the_output_transform() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        use klipp_core::{OdtChoice as O, WorkingColorspace as W};
        let cases = [
            (O::DciP3D65_48, W::AcesCG, "smpte432", "unknown", "bt709"),
            (O::Srgb, W::Rec709Linear, "bt709", "bt709", "bt709"),
            (O::MatchViewer, W::Rec709Linear, "bt709", "bt709", "bt709"),
            (O::MatchViewer, W::AcesCG, "bt709", "bt709", "bt709"),
            (O::Rec709Sdr, W::AcesCG, "bt709", "bt709", "bt709"),
            (O::Rec2020Sdr, W::AcesCG, "bt2020", "bt2020-10", "bt2020nc"),
            (O::Rec2020Sdr, W::Rec709Linear, "bt2020", "bt2020-10", "bt2020nc"),
        ];
        for codec in [ExportVideoCodec::H264, ExportVideoCodec::H265] {
            for (odt, working, primaries, transfer, space) in cases {
                let path = dir.join(format!("tags_{odt:?}_{working:?}_{codec:?}.mp4"));
                let mut settings = make_settings(codec, path.to_str().unwrap());
                settings.audio_codec = ExportAudioCodec::Copy;
                settings.odt = odt;
                let snapshot = SequenceSnapshot { working_colorspace: working, ..make_snapshot() };
                let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();
                let rgba = [90u8, 120, 150, 255].repeat(320 * 240);
                for _ in 0..3 {
                    enc.encode_video_frame(&rgba, 320, 240).unwrap();
                }
                enc.finalise().unwrap();
                let Ok(probe) = std::process::Command::new("ffprobe")
                    .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_primaries,color_transfer,color_space"])
                    .args(["-of", "default=nw=1"])
                    .arg(&path)
                    .output()
                else {
                    return;
                };
                let tags = String::from_utf8_lossy(&probe.stdout).to_string();
                let _ = std::fs::remove_file(&path);
                for want in [format!("color_primaries={primaries}"), format!("color_transfer={transfer}"), format!("color_space={space}")] {
                    assert!(tags.contains(&want), "{codec:?} {odt:?} in {working:?} lacks {want}: {tags}");
                }
            }
        }
    }

    /// #901: a Rec.2020 SDR export is BT.2020 in its pixels, not only its
    /// tags. Colour patches go in as linear BT.709 light; the file's
    /// Y'CbCr, decoded by the BT.2020 matrix its tags name, gives back
    /// what the ACES Rec.2020 SDR transform says each patch is, within
    /// 2.5 8-bit code values of compression; by the BT.709 matrix the
    /// primaries decode further off. Skips without ffmpeg.
    #[test]
    #[serial]
    fn a_rec2020_sdr_export_decodes_to_the_transform_by_the_bt2020_matrix() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("rec2020_sdr_patches.mp4");
        let mut settings = make_settings(ExportVideoCodec::H264, path.to_str().unwrap());
        settings.audio_codec = ExportAudioCodec::Copy;
        settings.crf = 4;
        settings.odt = klipp_core::OdtChoice::Rec2020Sdr;
        let snapshot = make_snapshot();
        let (w, h) = (320usize, 240usize);
        // Four 80-pixel-wide columns: grey, and BT.709's red, green and
        // blue, the most saturated a BT.709 project holds.
        let patches: [[f32; 3]; 4] = [[0.18, 0.18, 0.18], [0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]];
        let mut linear = vec![1.0f32; w * h * 4];
        for (i, px) in linear.chunks_exact_mut(4).enumerate() {
            px[..3].copy_from_slice(&patches[(i % w) / 80]);
        }
        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();
        for _ in 0..3 {
            enc.encode_linear_frame(&linear, w as u32, h as u32).unwrap();
        }
        enc.finalise().unwrap();
        let Ok(out) = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(&path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv444p", "-"])
            .output()
        else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        let raw = out.stdout;
        assert_eq!(raw.len(), w * h * 3, "one 4:4:4 frame: {}", String::from_utf8_lossy(&out.stderr));
        // Limited-range Y'CbCr back to R'G'B' by a matrix's luma weights.
        let decode = |at: usize, kr: f32, kb: f32| {
            let y = (raw[at] as f32 - 16.0) / 219.0;
            let cb = (raw[w * h + at] as f32 - 128.0) / 224.0;
            let cr = (raw[2 * w * h + at] as f32 - 128.0) / 224.0;
            let kg = 1.0 - kr - kb;
            let r = y + 2.0 * (1.0 - kr) * cr;
            let b = y + 2.0 * (1.0 - kb) * cb;
            let g = (y - kr * r - kb * b) / kg;
            [r, g, b].map(|v| v * 255.0)
        };
        let display = export_display(&settings, snapshot.working_colorspace);
        let mut bt709_off = 0.0f32;
        let mut matched = 0.0f32;
        for (i, p) in patches.iter().enumerate() {
            let at = (h / 2) * w + i * 80 + 40;
            let want = display.encode(*p).map(|v| v * 255.0);
            let got = decode(at, 0.2627, 0.0593);
            matched = matched.max((0..3).map(|c| (got[c] - want[c]).abs()).fold(0.0, f32::max));
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() <= 2.5, "patch {i} channel {c}: decoded {got:?}, the transform says {want:?}");
            }
            let wrong = decode(at, 0.2126, 0.0722);
            bt709_off = bt709_off.max((0..3).map(|c| (wrong[c] - want[c]).abs()).fold(0.0, f32::max));
        }
        // Measured: 1.6 codes by the BT.2020 matrix, 5.1 by BT.709's — the
        // ACES transform desaturates BT.709's primaries, so the matrices
        // differ by a few codes, not tens.
        assert!(matched <= 2.5, "decoded within {matched} of the transform");
        assert!(bt709_off > 4.0, "the BT.709 matrix should decode the primaries off, but is within {bt709_off}");
    }

    /// ProRes in MKV writes a Matroska file holding ProRes: it used to be
    /// forced into the MOV muxer whatever the extension said, and a MKV
    /// pair refused would fail a preset that exported before. Skips
    /// without ffprobe.
    #[test]
    #[serial]
    fn prores_in_mkv_is_written_as_matroska() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("prores.mkv");
        let mut settings = make_settings(ExportVideoCodec::ProRes422HQ, path.to_str().unwrap());
        settings.format = ExportFormat::Mkv;
        settings.audio_codec = ExportAudioCodec::Copy;
        let mut enc = Encoder::open(&path, &settings, &make_snapshot()).unwrap();
        let rgba = [90u8, 120, 150, 255].repeat(320 * 240);
        for _ in 0..3 {
            enc.encode_video_frame(&rgba, 320, 240).unwrap();
        }
        enc.finalise().unwrap();
        let Ok(probe) = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=format_name:stream=codec_name", "-of", "default=nw=1"])
            .arg(&path)
            .output()
        else {
            return;
        };
        let got = String::from_utf8_lossy(&probe.stdout).to_string();
        let _ = std::fs::remove_file(&path);
        assert!(got.contains("codec_name=prores") && got.contains("format_name=matroska"), "{got}");
    }

    /// A pair the container cannot hold is refused before anything is
    /// written, naming both halves.
    #[test]
    fn a_pair_the_container_cannot_hold_is_refused_before_writing() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let path = std::env::temp_dir().join("klipp_render_test").join("h264.webm");
        let mut settings = make_settings(ExportVideoCodec::H264, path.to_str().unwrap());
        settings.format = ExportFormat::WebM;
        let Err(RenderError::Encode(e)) = Encoder::open(&path, &settings, &make_snapshot()) else {
            panic!("H.264 in WebM was opened");
        };
        assert!(e.contains("WebM") && e.contains("H.264"), "{e}");
        assert!(!path.exists());
    }

    #[test]
    #[serial]
    fn encode_h265_video_only() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_h265.mp4");

        let mut settings = make_settings(ExportVideoCodec::H265, path.to_str().unwrap());
        settings.audio_codec = ExportAudioCodec::Copy; // no audio

        let snapshot = make_snapshot();

        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();

        let rgba = [0u8, 255, 0, 255].repeat(320 * 240); // green
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
        let _watch = crate::test_watchdog::Watchdog::start();
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
            audio_buses: Vec::new(),
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: w as u32,
            height: h as u32,
            sample_rate: 48000,
            duration_frames: 4,
            export_range: None,
            video: Default::default(),
            audio_tracks: vec![],
            pan_law: Default::default(),
            global_lut_path: None,
            subtitle_tracks: vec![],
            chapters: vec![],
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
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
