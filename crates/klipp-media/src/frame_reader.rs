//! A seekable single-frame reader: one open decoder, asked for frames at
//! arbitrary times.
//!
//! Both the viewer and export need frames of a clip's media at times
//! other than the one being shown — OFX plugins that read neighbours
//! (retime, frame blend, slit-scan), and in time the temporal repair
//! tools. The viewer had this; export did not, and reached for a
//! helper that opens the file, seeks, decodes and closes on every
//! single request.
//!
//! The part worth sharing is not the decode. It is the decision made
//! per request: seek, or read forward from where the decoder already
//! is. Walking forward a frame at a time — which is what export does,
//! and what a plugin asking for N-1, N, N+1 does — never seeks at all,
//! and seeking is what costs.
//!
//! Colour conversion stays with the caller. The viewer resolves a
//! frame's colourspace through its own tag-merging and camera-log
//! rules; export knows the log format from the media pool. Handing the
//! conversion in keeps those where they belong and keeps this a
//! reader.

use std::path::Path;

/// Turn a decoded frame into whatever the caller wants at the given
/// size — sRGB RGBA8 for an OFX plugin, linear RGBA f32 for anything
/// that has to sit beside the renderer's own frames.
pub type ConvertFnTo<T> =
    Box<dyn FnMut(&ffmpeg_next::frame::Video, u32, u32) -> Result<T, String> + Send>;

/// The common case.
pub type ConvertFn = ConvertFnTo<Vec<u8>>;

/// How many decoded frames to keep. A plugin reaching a couple of
/// frames either side of the one being rendered finds them here rather
/// than decoding twice.
const CACHE_FRAMES: usize = 8;

pub struct SeekableFrameReader<T = Vec<u8>> {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Video,
    stream_index: usize,
    frame: ffmpeg_next::frame::Video,
    /// Where the decoder is, in seconds. Negative before the first read.
    current_pts: f64,
    frame_duration: f64,
    cache: Vec<(f64, T, u32, u32)>,
    convert: ConvertFnTo<T>,
}

impl<T: Clone> SeekableFrameReader<T> {
    /// Open `path` and decode through `convert`.
    pub fn open(path: &Path, convert: ConvertFnTo<T>) -> Option<Self> {
        Self::open_with(path, |_, _| convert)
    }

    /// As [`Self::open`], but the converter is built from the open
    /// format context and the video stream's index.
    ///
    /// For callers whose conversion depends on the stream's colour
    /// tags. The alternative is opening the file a second time to read
    /// them, which is what this exists to avoid.
    pub fn open_with<F>(path: &Path, make_convert: F) -> Option<Self>
    where
        F: FnOnce(&ffmpeg_next::format::context::Input, usize) -> ConvertFnTo<T>,
    {
        let format_ctx = ffmpeg_next::format::input(path).ok()?;
        let (decoder, stream_index) = open_sw_decoder(&format_ctx).ok()?;
        let convert = make_convert(&format_ctx, stream_index);
        let frame_duration = format_ctx
            .streams()
            .best(ffmpeg_next::media::Type::Video)
            .map(|s| {
                let r = s.avg_frame_rate();
                if r.numerator() > 0 {
                    r.denominator() as f64 / r.numerator() as f64
                } else {
                    1.0 / 24.0
                }
            })
            .unwrap_or(1.0 / 24.0);
        Some(Self {
            format_ctx,
            decoder,
            stream_index,
            frame: ffmpeg_next::frame::Video::empty(),
            current_pts: -1.0,
            frame_duration,
            cache: Vec::new(),
            convert,
        })
    }

    /// The length of one frame, in seconds.
    pub fn frame_duration(&self) -> f64 {
        self.frame_duration
    }

    /// The frame at `seek_secs`, or `None` past the end of the file.
    pub fn decode_at(&mut self, seek_secs: f64) -> Option<(T, u32, u32)> {
        for (t, rgba, w, h) in &self.cache {
            if (*t - seek_secs).abs() < self.frame_duration * 0.5 {
                return Some((rgba.clone(), *w, *h));
            }
        }

        // Seek, or read forward from where the decoder already sits.
        // `||` rather than a chain of `if .. { true } else if ..`: there
        // is then no per-arm boolean to get wrong, and the reason for
        // each case still sits beside it.
        let need_seek = self.current_pts < 0.0                                  // first access
            || seek_secs < self.current_pts - self.frame_duration               // backwards
            || seek_secs > self.current_pts + self.frame_duration * 30.0;       // far forward

        if need_seek {
            let ts = ((seek_secs - 0.5).max(0.0) * 1_000_000.0) as i64;
            let _ = self.format_ctx.seek(ts, ..ts);
            self.decoder.flush();
            self.current_pts = seek_secs - 1.0; // approximate; the loop corrects it
        }

        for (stream, packet) in self.format_ctx.packets() {
            if stream.index() != self.stream_index {
                continue;
            }
            if self.decoder.send_packet(&packet).is_err() {
                while self.decoder.receive_frame(&mut self.frame).is_ok() {}
                if self.decoder.send_packet(&packet).is_err() {
                    tracing::warn!("SeekableFrameReader: packet lost after drain+retry");
                }
            }
            while self.decoder.receive_frame(&mut self.frame).is_ok() {
                let pts = self.frame.pts().unwrap_or(0);
                let tb = stream.time_base();
                let frame_secs = pts as f64 * tb.numerator() as f64 / tb.denominator() as f64;
                self.current_pts = frame_secs;
                if frame_secs < seek_secs - self.frame_duration * 0.5 {
                    continue;
                }
                let (w, h) = (self.frame.width(), self.frame.height());
                match (self.convert)(&self.frame, w, h) {
                    Ok(rgba) => {
                        if self.cache.len() >= CACHE_FRAMES {
                            self.cache.remove(0);
                        }
                        self.cache.push((frame_secs, rgba.clone(), w, h));
                        return Some((rgba, w, h));
                    }
                    Err(e) => {
                        tracing::warn!(
                            "SeekableFrameReader: the frame at {frame_secs:.3}s could not be converted: {e}"
                        );
                        return None;
                    }
                }
            }
        }
        // The file ended before the time asked for.
        None
    }
}

/// A source's colour, for turning its frames into sRGB the way the
/// viewer shows them ungraded.
///
/// The stream's tags are the baseline; a frame's own tags override
/// them where present; the camera log format has the last word,
/// because no file tag says "this is S-Log3".
///
/// The log format is passed in rather than looked up. The viewer keeps
/// a process-global registry of it, populated from the media pool, and
/// export knows it per clip as `MediaFile.log_format` — so a shared
/// lookup would be right for one caller and wrong for the other. Given
/// explicitly, the two agree by construction.
#[derive(Clone, Debug)]
pub struct SourceColour {
    baseline: klipp_core::SourceColorspace,
    log_format: Option<klipp_core::LogFormat>,
}

impl SourceColour {
    /// Read the baseline from an open stream.
    ///
    /// # Safety
    ///
    /// `format_ctx` must have a stream at `stream_index`.
    pub fn from_stream(
        format_ctx: &ffmpeg_next::format::context::Input,
        stream_index: usize,
        log_format: Option<klipp_core::LogFormat>,
    ) -> Self {
        let baseline = format_ctx
            .stream(stream_index)
            .map(|st| unsafe { stream_colour_baseline((*st.as_ptr()).codecpar) })
            .unwrap_or_else(|| unsafe { stream_colour_baseline(std::ptr::null()) });
        Self { baseline, log_format }
    }

    pub fn with_baseline(
        baseline: klipp_core::SourceColorspace,
        log_format: Option<klipp_core::LogFormat>,
    ) -> Self {
        Self { baseline, log_format }
    }

    /// This frame's colourspace: its own tags over the baseline, then
    /// the log format.
    ///
    /// # Safety
    ///
    /// `frame` must be a valid AVFrame.
    pub unsafe fn colourspace_of(
        &self,
        frame: *const ffmpeg_next::ffi::AVFrame,
    ) -> klipp_core::SourceColorspace {
        let f = &*frame;
        let cs = klipp_core::SourceColorspace::resolve_with_baseline(
            f.color_range as i32,
            f.colorspace as i32,
            f.color_primaries as i32,
            f.color_trc as i32,
            self.baseline,
        );
        match self.log_format {
            Some(fmt) => fmt.apply_to(cs),
            None => cs,
        }
    }

    /// sRGB RGBA8 for a scene-referred frame (PQ, HLG, camera log);
    /// `None` for a display-referred one, which keeps swscale.
    ///
    /// # Safety
    ///
    /// `frame` must be a valid software AVFrame.
    pub unsafe fn scene_referred_rgba8(
        &self,
        frame: *const ffmpeg_next::ffi::AVFrame,
        dst_w: u32,
        dst_h: u32,
    ) -> Option<Vec<u8>> {
        let cs = self.colourspace_of(frame);
        let ids = klipp_core::color_ids::ids(cs);
        let transfer = klipp_color::hdr::HdrTransfer::from_transfer_id(ids[2])?;
        let yuv = crate::yuv_samples::read_yuv_samples(frame)?;
        let codes = klipp_color::hdr::yuv_to_hdr_codes(&yuv.samples(), dst_w, dst_h, 0, ids);
        let m = klipp_color::camera_log::primaries_to_working_matrix(
            cs.primaries,
            klipp_core::WorkingColorspace::Rec709Linear,
        );
        Some(klipp_color::hdr::hdr10_codes_to_display_srgb8(&codes, transfer, m))
    }
}

impl SourceColour {
    /// Linear RGBA f32 in `working`, the way the renderer's own path
    /// renders this source.
    ///
    /// The point of it: a frame read at another time has to be the
    /// same rendering as the frame beside it, or anything comparing
    /// them is comparing two pictures rather than two samples. A
    /// scene-referred source (PQ, HLG, camera log) goes to *scene*
    /// linear, which is what `to_linear` produces for it — not to the
    /// display sRGB [`Self::scene_referred_rgba8`] gives a plugin.
    ///
    /// # Safety
    ///
    /// `frame` must be a valid software AVFrame.
    pub unsafe fn scene_referred_linear(
        &self,
        frame: *const ffmpeg_next::ffi::AVFrame,
        dst_w: u32,
        dst_h: u32,
        working: klipp_core::WorkingColorspace,
    ) -> Option<Vec<f32>> {
        let cs = self.colourspace_of(frame);
        let ids = klipp_core::color_ids::ids(cs);
        let transfer = klipp_color::hdr::HdrTransfer::from_transfer_id(ids[2])?;
        let yuv = crate::yuv_samples::read_yuv_samples(frame)?;
        let codes = klipp_color::hdr::yuv_to_hdr_codes(&yuv.samples(), dst_w, dst_h, 0, ids);
        let m = klipp_color::camera_log::primaries_to_working_matrix(cs.primaries, working);
        Some(klipp_color::hdr::hdr10_codes_to_scene_linear_rgba(&codes, transfer, m))
    }
}

impl SeekableFrameReader<Vec<u8>> {
    /// Open `path` converting scene-referred sources (PQ, HLG, camera
    /// log) as the viewer shows them ungraded, and everything else
    /// with swscale.
    ///
    /// This is what a caller wants unless it has colour rules of its
    /// own: the alternative renders log flat and PQ washed out.
    pub fn open_colour_managed(
        path: &Path,
        log_format: Option<klipp_core::LogFormat>,
    ) -> Option<Self> {
        Self::open_with(path, |format_ctx, stream_index| {
            let colour = SourceColour::from_stream(format_ctx, stream_index, log_format);
            Box::new(move |frame: &ffmpeg_next::frame::Video, w: u32, h: u32| {
                if let Some(rgba) = unsafe { colour.scene_referred_rgba8(frame.as_ptr(), w, h) } {
                    return Ok(rgba);
                }
                swscale_rgba(frame, w, h)
            })
        })
    }

    /// Open `path` converting with plain swscale to RGBA.
    ///
    /// For callers with no colour rules of their own. A scene-referred
    /// source (PQ, HLG, camera log) comes back as swscale renders it,
    /// which is flat for log and washed out for PQ — see the issue on
    /// the two paths disagreeing about colour.
    pub fn open_rgba(path: &Path) -> Option<Self> {
        Self::open(path, Box::new(swscale_rgba))
    }

}

impl SeekableFrameReader<Vec<f32>> {
    /// Open `path` returning linear RGBA f32 in `working`.
    ///
    /// Scene-referred sources go through their own decode to scene
    /// linear; everything else is swscale to sRGB, decoded, and put
    /// into the working space. Either way the result is what the
    /// renderer's `to_linear` would have produced for the same frame.
    pub fn open_linear(
        path: &Path,
        log_format: Option<klipp_core::LogFormat>,
        working: klipp_core::WorkingColorspace,
    ) -> Option<Self> {
        // BT.709 is what swscale hands back, and the authored colours
        // of a display-referred source are in it.
        let to_working = klipp_color::primaries::working_space_conversion_matrix(
            klipp_core::WorkingColorspace::Rec709Linear,
            working,
        );
        Self::open_with(path, move |format_ctx, stream_index| {
            let colour = SourceColour::from_stream(format_ctx, stream_index, log_format);
            Box::new(move |frame: &ffmpeg_next::frame::Video, w: u32, h: u32| {
                if let Some(linear) =
                    unsafe { colour.scene_referred_linear(frame.as_ptr(), w, h, working) }
                {
                    return Ok(linear);
                }
                let rgba = swscale_rgba(frame, w, h)?;
                let mut out: Vec<f32> = rgba
                    .iter()
                    .map(|&c| klipp_color::transfer::srgb_to_linear(c as f32 / 255.0))
                    .collect();
                // Alpha is linear already; the decode above bent it.
                for (i, px) in rgba.chunks_exact(4).enumerate() {
                    out[i * 4 + 3] = px[3] as f32 / 255.0;
                }
                if let Some(m) = to_working {
                    out.chunks_exact_mut(4).for_each(|px| {
                        let c = klipp_color::primaries::matrix3_mul_vec(m, [px[0], px[1], px[2]]);
                        px[0] = c[0];
                        px[1] = c[1];
                        px[2] = c[2];
                    });
                }
                Ok(out)
            })
        })
    }
}

/// A stream's colour tags from its codec parameters: the baseline a
/// frame's own tags override. NVDEC downloads often drop per-frame
/// tags, so this is what makes e.g. a full-range file decode as full
/// range.
///
/// # Safety
///
/// `codecpar` must be valid or null.
pub unsafe fn stream_colour_baseline(
    codecpar: *const ffmpeg_next::ffi::AVCodecParameters,
) -> klipp_core::SourceColorspace {
    if codecpar.is_null() {
        return klipp_core::SourceColorspace::resolve(0, 0, 0, 0, 1920, 1080);
    }
    let p = &*codecpar;
    klipp_core::SourceColorspace::resolve(
        p.color_range as i32,
        p.color_space as i32,
        p.color_primaries as i32,
        p.color_trc as i32,
        p.width.max(1) as u32,
        p.height.max(1) as u32,
    )
}

/// Plain swscale to RGBA at the requested size.
pub fn swscale_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, String> {
    use ffmpeg_next::software::scaling;
    let (fmt, w, h) = (frame.format(), frame.width(), frame.height());
    let mut scaler = scaling::Context::get(
        fmt,
        w,
        h,
        ffmpeg_next::format::Pixel::RGBA,
        dst_w,
        dst_h,
        scaling::Flags::BILINEAR,
    )
    .map_err(|e| format!("swscale has no path from {fmt:?} {w}x{h} to RGBA {dst_w}x{dst_h}: {e}"))?;
    let mut rgba = ffmpeg_next::frame::Video::empty();
    scaler
        .run(frame, &mut rgba)
        .map_err(|e| format!("swscale failed converting {fmt:?} {w}x{h} to RGBA: {e}"))?;
    Ok(packed_rgba(&rgba))
}

/// Copy a scaled RGBA frame out of ffmpeg's padded buffer.
///
/// ffmpeg pads each row to its own stride; a caller wants `w * 4` bytes
/// per row and nothing between them.
fn packed_rgba(frame: &ffmpeg_next::frame::Video) -> Vec<u8> {
    // The scaled frame's own dimensions, not what was asked for:
    // swscale is the authority on what it produced, and reading a row
    // length the frame does not have is how a stride bug becomes a
    // skewed picture rather than an error.
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let stride = frame.stride(0);
    let row = w * 4;
    let data = frame.data(0);
    if stride == row {
        return data[..row * h].to_vec();
    }
    let mut out = Vec::with_capacity(row * h);
    for y in 0..h {
        let start = y * stride;
        out.extend_from_slice(&data[start..start + row]);
    }
    out
}

/// A software video decoder for `format_ctx`'s best video stream.
fn open_sw_decoder(
    format_ctx: &ffmpeg_next::format::context::Input,
) -> Result<(ffmpeg_next::decoder::Video, usize), String> {
    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or("no video stream")?;
    let stream_index = video_stream.index();
    let mut codec_ctx =
        ffmpeg_next::codec::context::Context::from_parameters(video_stream.parameters())
            .map_err(|e| format!("codec context: {e}"))?;
    unsafe {
        let cores = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as i32).min(16);
        (*codec_ctx.as_mut_ptr()).thread_count = cores;
    }
    let decoder = codec_ctx
        .decoder()
        .video()
        .map_err(|e| format!("video decoder: {e}"))?;
    Ok((decoder, stream_index))
}
