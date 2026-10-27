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
        let format_ctx = crate::open_input(path).ok()?;
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
        // Backwards is any frame before the one the decoder sits on: it
        // was read and passed (not kept, above), so reading on gives a
        // later frame, never it.
        let need_seek = self.current_pts < 0.0                                  // first access
            || seek_secs < self.current_pts - self.frame_duration * 0.5         // backwards
            || seek_secs > self.current_pts + self.frame_duration * 30.0;       // far forward

        if need_seek {
            let ts = ((seek_secs - 0.5).max(0.0) * 1_000_000.0) as i64;
            let _ = self.format_ctx.seek(ts, ..ts);
            self.decoder.flush();
            self.current_pts = seek_secs - 1.0; // approximate; the loop corrects it
        }

        let tb = self.format_ctx.stream(self.stream_index).map_or(0.0, |st| {
            let tb = st.time_base();
            tb.numerator() as f64 / tb.denominator().max(1) as f64
        });
        // A packet at a time, so the format context is free again for
        // the frames it gives.
        while let Some((index, packet)) = self.format_ctx.packets().next().map(|(st, p)| (st.index(), p)) {
            if index != self.stream_index {
                continue;
            }
            if self.decoder.send_packet(&packet).is_err() {
                while self.decoder.receive_frame(&mut self.frame).is_ok() {}
                if self.decoder.send_packet(&packet).is_err() {
                    tracing::warn!("SeekableFrameReader: packet lost after drain+retry");
                }
            }
            if let Some(found) = self.receive(seek_secs, tb) {
                return found;
            }
        }
        // The packets are spent, but a decoder with frame threads still
        // holds the last frames back: at the file's end they are drained
        // (a second end of file only errors, and the frames still held
        // come out as before). A seek flushes the decoder for the next.
        let _ = self.decoder.send_eof();
        if let Some(found) = self.receive(seek_secs, tb) {
            return found;
        }
        // The file ended before the time asked for.
        None
    }

    /// Every frame from the one at `from_secs` on, in the file's order,
    /// each with its time as [`Self::decode_at`] names it (the stream's
    /// pts in seconds, the time the render asks a frame for by). For a
    /// pass over a range (#239): no seek after the first, and a frame's
    /// time is its own, not counted, so a variable frame rate or a
    /// stream that starts late is named as the render reads it.
    pub fn into_frames_from(self, from_secs: f64) -> FramesFrom<T> {
        FramesFrom { reader: self, from_secs, started: false, ended: false }
    }

    /// The next frame the decoder gives, after a packet at a time; at the
    /// file's end, what it still holds. `None` when nothing is left.
    fn next_decoded(&mut self, tb: f64, ended: &mut bool) -> Option<Result<(f64, T, u32, u32), String>> {
        loop {
            if self.decoder.receive_frame(&mut self.frame).is_ok() {
                let secs = self.frame.pts().unwrap_or(0) as f64 * tb;
                self.current_pts = secs;
                let (w, h) = (self.frame.width(), self.frame.height());
                return Some((self.convert)(&self.frame, w, h).map(|t| (secs, t, w, h)));
            }
            if *ended {
                return None;
            }
            match self.format_ctx.packets().next().map(|(st, p)| (st.index(), p)) {
                Some((index, packet)) => {
                    if index == self.stream_index && self.decoder.send_packet(&packet).is_err() {
                        while self.decoder.receive_frame(&mut self.frame).is_ok() {}
                        let _ = self.decoder.send_packet(&packet);
                    }
                }
                None => {
                    let _ = self.decoder.send_eof();
                    *ended = true;
                }
            }
        }
    }

    /// The decoded frames the decoder gives now: `Some` with the answer
    /// once one is at or after `seek_secs` (time base `tb`, seconds a
    /// tick), `None` while it has given only earlier ones.
    fn receive(&mut self, seek_secs: f64, tb: f64) -> Option<Option<(T, u32, u32)>> {
        while self.decoder.receive_frame(&mut self.frame).is_ok() {
            let frame_secs = self.frame.pts().unwrap_or(0) as f64 * tb;
            self.current_pts = frame_secs;
            if frame_secs < seek_secs - self.frame_duration * 0.5 {
                continue;
            }
            let (w, h) = (self.frame.width(), self.frame.height());
            return Some(match (self.convert)(&self.frame, w, h) {
                Ok(rgba) => {
                    if self.cache.len() >= CACHE_FRAMES {
                        self.cache.remove(0);
                    }
                    self.cache.push((frame_secs, rgba.clone(), w, h));
                    Some((rgba, w, h))
                }
                Err(e) => {
                    tracing::warn!("SeekableFrameReader: the frame at {frame_secs:.3}s could not be converted: {e}");
                    None
                }
            });
        }
        None
    }
}

/// The frames of [`SeekableFrameReader::into_frames_from`], with their
/// times.
pub struct FramesFrom<T> {
    reader: SeekableFrameReader<T>,
    from_secs: f64,
    started: bool,
    ended: bool,
}

impl<T: Clone> FramesFrom<T> {
    /// The length of one frame, in seconds.
    pub fn frame_duration(&self) -> f64 {
        self.reader.frame_duration
    }
}

impl<T: Clone> Iterator for FramesFrom<T> {
    type Item = Result<(f64, T, u32, u32), String>;

    fn next(&mut self) -> Option<Self::Item> {
        let r = &mut self.reader;
        let tb = r.format_ctx.stream(r.stream_index).map_or(0.0, |st| {
            let tb = st.time_base();
            tb.numerator() as f64 / tb.denominator().max(1) as f64
        });
        if !self.started {
            self.started = true;
            // As `decode_at` seeks: half a second before, then on to the
            // first frame that is the one asked for (half a frame's grace).
            let ts = ((self.from_secs - 0.5).max(0.0) * 1_000_000.0) as i64;
            let _ = r.format_ctx.seek(ts, ..ts);
            r.decoder.flush();
            r.cache.clear();
            loop {
                match r.next_decoded(tb, &mut self.ended)? {
                    Ok((secs, ..)) if secs < self.from_secs - r.frame_duration * 0.5 => continue,
                    other => return Some(other),
                }
            }
        }
        r.next_decoded(tb, &mut self.ended)
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
        Some(codes.to_display_srgb8(transfer, m))
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
        Some(codes.to_scene_linear_rgba(transfer, m))
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
                if let Some(rgba) = display_referred_rgba8(&colour, frame, w, h) {
                    return Ok(rgba);
                }
                swscale_rgba(frame, w, h)
            })
        })
    }

    /// [`Self::open_colour_managed`], every frame scaled to `w` × `h`
    /// whatever its own size: a pass that wants the picture small (#239).
    pub fn open_colour_managed_sized(path: &Path, log_format: Option<klipp_core::LogFormat>, w: u32, h: u32) -> Option<Self> {
        Self::open_with(path, |format_ctx, stream_index| {
            let colour = SourceColour::from_stream(format_ctx, stream_index, log_format);
            Box::new(move |frame: &ffmpeg_next::frame::Video, _: u32, _: u32| {
                if let Some(rgba) = unsafe { colour.scene_referred_rgba8(frame.as_ptr(), w, h) } {
                    return Ok(rgba);
                }
                if let Some(rgba) = display_referred_rgba8(&colour, frame, w, h) {
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
    /// linear. Display-referred Y'CbCr converts as the viewer's software
    /// decode does: native depth, the file's matrix, range, transfer and
    /// primaries, into the working space (#1995). Only a frame with no
    /// Y'CbCr (RGB, greyscale) goes through swscale to sRGB and is
    /// decoded from there. See `linear_convert`.
    pub fn open_linear(
        path: &Path,
        log_format: Option<klipp_core::LogFormat>,
        working: klipp_core::WorkingColorspace,
    ) -> Option<Self> {
        Self::open_with(path, move |format_ctx, stream_index| linear_convert(format_ctx, stream_index, log_format, working))
    }
}

/// Linear readers of other media items' files, held across frames (a
/// media source, #1746): a render walking forward reads each one on
/// without a seek. At most `cap` are open, the one read longest ago
/// closed first, so a project with many sources holds few decoders.
pub struct MediaReaders {
    cap: usize,
    /// Least recently read first.
    open: Vec<(std::path::PathBuf, Option<klipp_core::LogFormat>, klipp_core::WorkingColorspace, SeekableFrameReader<Vec<f32>>)>,
}

impl MediaReaders {
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), open: Vec::new() }
    }

    /// `path`'s frame at `secs`, linear in `working`, as
    /// [`SeekableFrameReader::open_linear`] decodes it. `None` when the
    /// file won't open or has no frame there.
    pub fn read(&mut self, path: &Path, log_format: Option<klipp_core::LogFormat>, working: klipp_core::WorkingColorspace, secs: f64) -> Option<(Vec<f32>, u32, u32)> {
        match self.open.iter().position(|(p, l, w, _)| p == path && *l == log_format && *w == working) {
            Some(i) => {
                let held = self.open.remove(i);
                self.open.push(held);
            }
            None => {
                let reader = SeekableFrameReader::open_linear(path, log_format, working)?;
                if self.open.len() >= self.cap {
                    self.open.remove(0);
                }
                self.open.push((path.to_owned(), log_format, working, reader));
            }
        }
        self.open.last_mut()?.3.decode_at(secs.max(0.0))
    }

    /// Whether a reader of `path` is open.
    pub fn is_open(&self, path: &Path) -> bool {
        self.open.iter().any(|(p, ..)| p == path)
    }

    /// How many readers are open.
    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

/// The conversion [`SeekableFrameReader::open_linear`] and
/// [`BackwardFrameReader::open_linear`] decode through.
///
/// A display-referred Y'CbCr frame converts exactly as the viewer's
/// software decode converts it (`video_decoder::convert_sw_frame`): its
/// samples read at native depth, the frame's own matrix, range,
/// transfer and primaries (its tags over the stream's), chroma at the
/// decode shaders' siting. A temporal effect compares the frames this
/// reads with the one it is given, so a neighbour has to be the same
/// rendering as that frame, not merely the same picture: this used to
/// go through 8-bit swscale with swscale's default matrix, BT.601, and
/// a BT.709 neighbour came back up to 59 levels off its own frame
/// (#1995) — more than the change from one frame to the next.
fn linear_convert(
    format_ctx: &ffmpeg_next::format::context::Input,
    stream_index: usize,
    log_format: Option<klipp_core::LogFormat>,
    working: klipp_core::WorkingColorspace,
) -> ConvertFnTo<Vec<f32>> {
    // BT.709 is what swscale hands back, and the authored colours
    // of a display-referred source are in it.
    let to_working = klipp_color::primaries::working_space_conversion_matrix(
        klipp_core::WorkingColorspace::Rec709Linear,
        working,
    );
    let colour = SourceColour::from_stream(format_ctx, stream_index, log_format);
    Box::new(move |frame: &ffmpeg_next::frame::Video, w: u32, h: u32| {
        if let Some(linear) =
            unsafe { colour.scene_referred_linear(frame.as_ptr(), w, h, working) }
        {
            return Ok(linear);
        }
        if let Some(linear) = display_referred_linear(&colour, frame, w, h, working) {
            return Ok(linear);
        }
        // What is left has no Y'CbCr to read: RGB, palette, greyscale.
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
}

/// A display-referred Y'CbCr frame in linear light, `working` primaries,
/// as the viewer's software decode gives it: the 10-bit 4:2:0 converter
/// for `YUV420P10LE`, the general one for every other Y'CbCr layout.
/// `None` for a frame with no Y'CbCr samples to read.
fn display_referred_linear(
    colour: &SourceColour,
    frame: &ffmpeg_next::frame::Video,
    w: u32,
    h: u32,
    working: klipp_core::WorkingColorspace,
) -> Option<Vec<f32>> {
    // SAFETY: a decoded frame's own AVFrame.
    let cs = unsafe { colour.colourspace_of(frame.as_ptr()) };
    if frame.format() == ffmpeg_next::format::Pixel::YUV420P10LE {
        return Some(klipp_color::camera_log::yuv420p10le_to_linear(
            frame.data(0),
            frame.data(1),
            frame.data(2),
            frame.stride(0),
            frame.stride(1),
            frame.stride(2),
            frame.width(),
            frame.height(),
            w,
            h,
            cs,
            working,
        ));
    }
    // SAFETY: as above.
    let yuv = unsafe { crate::yuv_samples::read_yuv_samples(frame.as_ptr()) }?;
    Some(klipp_color::camera_log::yuv_to_linear_display(&yuv.samples(), w, h, 0, cs, working))
}

/// A display-referred Y'CbCr frame as sRGB-encoded 8-bit RGBA, the form
/// an OFX plug-in is given its own frame in (#2013): decoded as
/// [`display_referred_linear`] decodes it (the frame's matrix, range and
/// primaries, at native depth) in Rec.709 linear, then encoded by
/// [`klipp_color::ofx::encode`], the plug-in's own frame's encoder. Through
/// plain swscale it took BT.601 whatever the file said, so a plug-in's
/// neighbours on BT.709 footage were another colour from its frame.
/// `None` for a frame with no Y'CbCr samples to read.
fn display_referred_rgba8(colour: &SourceColour, frame: &ffmpeg_next::frame::Video, w: u32, h: u32) -> Option<Vec<u8>> {
    let linear = display_referred_linear(colour, frame, w, h, klipp_core::WorkingColorspace::Rec709Linear)?;
    Some(klipp_color::ofx::encode(&linear, klipp_core::WorkingColorspace::Rec709Linear))
}

/// A picture of `sw`×`sh` fitted into a `fw`×`fh` frame, aspect kept:
/// the frame itself when the two have the same shape (to within a pixel
/// of rounding), else the frame's full height or width.
pub fn fitted_size(sw: u32, sh: u32, fw: u32, fh: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 {
        return (fw, fh);
    }
    let (sw, sh, fw64, fh64) = (sw as u64, sh as u64, fw as u64, fh as u64);
    // Width at the frame's height, and the other way round.
    let w_at_fh = (sw * fh64 + sh / 2) / sh;
    let h_at_fw = (sh * fw64 + sw / 2) / sw;
    if w_at_fh.abs_diff(fw64) <= 1 || h_at_fw.abs_diff(fh64) <= 1 {
        return (fw, fh);
    }
    // Not rounded to even: an odd picture, 1920x803, decodes at its own
    // 803 rows, as the viewer's does, so the bars fall on the same rows.
    let side = |v: u64| (v as u32).max(1);
    if w_at_fh < fw64 {
        (side(w_at_fh), fh)
    } else {
        (fw, side(h_at_fw))
    }
}

/// An RGBA frame turned upright by a multiple of 90°: gathered row by row
/// from the source, as the export decoder turns its frames.
fn rotate_rgba_f32(rgba: &[f32], w: u32, h: u32, degrees: i32) -> (Vec<f32>, u32, u32) {
    use rayon::prelude::*;
    let (nw, nh) = match degrees {
        90 | 270 => (h, w),
        180 => (w, h),
        _ => return (rgba.to_vec(), w, h),
    };
    let mut out = vec![0.0f32; (nw * nh * 4) as usize];
    let (wi, hi, nwi) = (w as usize, h as usize, nw as usize);
    out.par_chunks_exact_mut(nwi * 4).enumerate().for_each(|(dy, row)| {
        for dx in 0..nwi {
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

/// How many decoded frames a [`BackwardFrameReader`] window holds: about
/// a second of footage, the stretch one seek and one forward decode serve.
const BACKWARD_WINDOW_FRAMES: usize = 32;

/// Decoded frames, oldest first, by presentation time; and whether they
/// run to the end of the file.
struct Window {
    frames: Vec<(f64, ffmpeg_next::frame::Video)>,
    at_end: bool,
}

impl Window {
    /// Whether the window answers `secs` without decoding.
    fn covers(&self, secs: f64, frame_duration: f64) -> bool {
        let (Some((first, _)), Some((last, _))) = (self.frames.first(), self.frames.last()) else {
            return false;
        };
        let half = frame_duration * 0.5;
        // Before the window's first frame is the window before — unless
        // that frame is the file's first.
        let from = if *first <= half { f64::NEG_INFINITY } else { first - half };
        let to = if self.at_end { f64::INFINITY } else { last + half };
        secs >= from && secs <= to
    }
}

/// The decoding half of a [`BackwardFrameReader`]: one open file, asked
/// for the window of frames that ends at a time. It runs on a thread of
/// its own, so the window before the one being shown is decoded while
/// that one plays.
struct WindowDecoder {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Video,
    stream_index: usize,
    time_base: f64,
    frame_duration: f64,
}

impl WindowDecoder {
    /// The window of frames that ends at `secs`: seek a window's length
    /// before it, and read forward to it.
    fn window_up_to(&mut self, secs: f64) -> Window {
        let half = self.frame_duration * 0.5;
        let slack = self.frame_duration * 0.01;
        let start = (secs - self.frame_duration * (BACKWARD_WINDOW_FRAMES as f64 - 1.0)).max(0.0);
        let ts = (start * 1_000_000.0) as i64;
        // To the keyframe at or before the start, as every seek here.
        let _ = self.format_ctx.seek(ts, ..ts);
        self.decoder.flush();
        let mut frames: Vec<(f64, ffmpeg_next::frame::Video)> = Vec::new();
        // The latest frame passed over before the window: a time past the
        // file's end, whose window would hold nothing, shows the file's
        // last frame.
        let mut before: Option<(f64, ffmpeg_next::frame::Video)> = None;
        // Keep a frame; `true` once past the time asked for.
        let mut keep = |pts: f64, frame: ffmpeg_next::frame::Video, frames: &mut Vec<(f64, ffmpeg_next::frame::Video)>| -> bool {
            if pts > secs + slack {
                // Past the time asked for: the window ends before it,
                // unless it would be empty — a time before the file's
                // first frame shows that frame.
                if frames.is_empty() {
                    frames.push((pts, frame));
                }
                return true;
            }
            if pts >= start - half {
                frames.push((pts, frame));
                if frames.len() > BACKWARD_WINDOW_FRAMES {
                    frames.remove(0);
                }
            } else if before.as_ref().is_none_or(|(p, _)| pts > *p) {
                before = Some((pts, frame));
            }
            false
        };
        let (stream_index, time_base) = (self.stream_index, self.time_base);
        let mut done = false;
        for (stream, packet) in self.format_ctx.packets() {
            if stream.index() != stream_index {
                continue;
            }
            if self.decoder.send_packet(&packet).is_err() {
                continue;
            }
            let mut frame = ffmpeg_next::frame::Video::empty();
            while self.decoder.receive_frame(&mut frame).is_ok() {
                let pts = frame.timestamp().or(frame.pts()).unwrap_or(0) as f64 * time_base;
                if keep(pts, std::mem::replace(&mut frame, ffmpeg_next::frame::Video::empty()), &mut frames) {
                    done = true;
                    break;
                }
            }
            if done {
                break;
            }
        }
        let mut at_end = false;
        if !done {
            // The file ended first. The decoder still holds its last
            // frames — a reversed clip's first ones, often — until told.
            let _ = self.decoder.send_eof();
            let mut frame = ffmpeg_next::frame::Video::empty();
            while self.decoder.receive_frame(&mut frame).is_ok() {
                let pts = frame.timestamp().or(frame.pts()).unwrap_or(0) as f64 * time_base;
                if keep(pts, std::mem::replace(&mut frame, ffmpeg_next::frame::Video::empty()), &mut frames) {
                    done = true;
                    break;
                }
            }
            at_end = !done;
        }
        if frames.is_empty() {
            frames.extend(before);
        }
        // Frames come out in presentation order; a stream that does not
        // keep it is put in it.
        frames.sort_by(|a, b| a.0.total_cmp(&b.0));
        Window { frames, at_end }
    }
}

/// Frames of a file asked for at falling times: a reversed clip (#617).
///
/// Every decoder reads forward, and [`SeekableFrameReader`] seeks on any
/// step back, so walking a clip backward a frame at a time seeks and
/// decodes from a keyframe for every frame shown. This decodes a window
/// instead — the second or so of frames up to the time asked for, from
/// one seek — and answers the following, earlier times from it. While
/// it does, the window before is decoded on a thread of its own, so the
/// next one is there when the frames run off this one's start: a window
/// decoded where it is needed stalls the picture for a quarter to half a
/// second at 4K.
///
/// The windows hold frames as the decoder produced them, a fraction of
/// their converted size; each is converted when it is shown, and the
/// last conversion is kept for a frame shown again.
pub struct BackwardFrameReader<T = Vec<f32>> {
    frame_duration: f64,
    /// The video's coded size.
    coded: (u32, u32),
    window: Window,
    /// The window decoding ahead, by the time it ends at, once asked for.
    pending: Option<f64>,
    /// A window decoded ahead and not yet shown from.
    ahead: Option<(f64, Window)>,
    requests: std::sync::mpsc::Sender<f64>,
    windows: std::sync::mpsc::Receiver<(f64, Window)>,
    /// The last frame converted: its time, size and pixels.
    last: Option<(f64, u32, u32, T)>,
    convert: ConvertFnTo<T>,
    /// Windows decoded so far: each one seek.
    fills: usize,
}

impl BackwardFrameReader<Vec<f32>> {
    /// Open `path` returning linear RGBA f32 in `working`, converted as
    /// [`SeekableFrameReader::open_linear`] converts.
    pub fn open_linear(
        path: &Path,
        log_format: Option<klipp_core::LogFormat>,
        working: klipp_core::WorkingColorspace,
    ) -> Option<Self> {
        let format_ctx = crate::open_input(path).ok()?;
        let (decoder, stream_index) = open_sw_decoder(&format_ctx).ok()?;
        let convert = linear_convert(&format_ctx, stream_index, log_format, working);
        let stream = format_ctx.stream(stream_index)?;
        let tb = stream.time_base();
        let time_base = tb.numerator() as f64 / tb.denominator().max(1) as f64;
        let r = stream.avg_frame_rate();
        let frame_duration = if r.numerator() > 0 { r.denominator() as f64 / r.numerator() as f64 } else { 1.0 / 24.0 };
        let coded = (decoder.width(), decoder.height());
        let mut core = WindowDecoder { format_ctx, decoder, stream_index, time_base, frame_duration };
        let (requests, asked) = std::sync::mpsc::channel::<f64>();
        let (made, windows) = std::sync::mpsc::channel::<(f64, Window)>();
        std::thread::Builder::new()
            .name("klipp-backward".into())
            .spawn(move || {
                // Until the reader is dropped: its sender goes with it.
                while let Ok(secs) = asked.recv() {
                    if made.send((secs, core.window_up_to(secs))).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            frame_duration,
            coded,
            window: Window { frames: Vec::new(), at_end: false },
            pending: None,
            ahead: None,
            requests,
            windows,
            last: None,
            convert,
            fills: 0,
        })
    }

    /// [`Self::decode_at`], the picture turned upright by `rotation`
    /// (degrees, as the decoders take it) and fitted into `frame` with its
    /// aspect kept, as a clip's forward decoder hands it over.
    pub fn decode_upright(&mut self, secs: f64, frame: (u32, u32), rotation: i32) -> Option<(Vec<f32>, u32, u32, f64)> {
        let rotation = rotation.rem_euclid(360);
        let turned = matches!(rotation, 90 | 270);
        let (cw, ch) = self.coded;
        let (uw, uh) = if turned { (ch, cw) } else { (cw, ch) };
        let (w, h) = fitted_size(uw, uh, frame.0, frame.1);
        let coded = if turned { (h, w) } else { (w, h) };
        let (pixels, pw, ph, pts) = self.decode_at(secs, Some(coded))?;
        if rotation == 0 {
            return Some((pixels, pw, ph, pts));
        }
        let (pixels, w, h) = rotate_rgba_f32(&pixels, pw, ph, rotation);
        Some((pixels, w, h, pts))
    }
}

impl<T: Clone> BackwardFrameReader<T> {
    /// How many windows have been decoded, each from one seek.
    pub fn fills(&self) -> usize {
        self.fills
    }

    /// The length of one frame, in seconds.
    pub fn frame_duration(&self) -> f64 {
        self.frame_duration
    }

    /// The frame showing at `secs` — the latest that starts by then, or
    /// the file's first before it — at `size` when given, else at its
    /// own; and its presentation time. `None` when nothing decodes.
    pub fn decode_at(&mut self, secs: f64, size: Option<(u32, u32)>) -> Option<(T, u32, u32, f64)> {
        if !self.window.covers(secs, self.frame_duration) {
            self.window = self.window_for(secs)?;
        }
        self.ask_ahead();
        // The frame that has started by `secs`, with a hundredth of a frame
        // of slack for the arithmetic: a reversed clip asks for the times
        // a forward one would, mirrored, and the mirror only lands on the
        // same frames if both take the one that is showing, not the
        // nearest.
        let slack = self.frame_duration * 0.01;
        let i = self.window.frames.iter().rposition(|(pts, _)| *pts <= secs + slack).unwrap_or(0);
        let (pts, frame) = self.window.frames.get(i)?;
        let (w, h) = size.unwrap_or((frame.width(), frame.height()));
        if let Some((p, lw, lh, pixels)) = &self.last {
            if (*p - *pts).abs() < 1e-9 && (*lw, *lh) == (w, h) {
                return Some((pixels.clone(), w, h, *pts));
            }
        }
        let pts = *pts;
        match (self.convert)(frame, w, h) {
            Ok(pixels) => {
                self.last = Some((pts, w, h, pixels.clone()));
                Some((pixels, w, h, pts))
            }
            Err(e) => {
                tracing::warn!("BackwardFrameReader: the frame at {pts:.3}s could not be converted: {e}");
                None
            }
        }
    }

    /// The window that answers `secs`: the one decoded ahead when it
    /// does, the one decoding when it will, else one decoded now. A
    /// window asked for this very time is the answer even when it does
    /// not cover it — the nearest the file has — or asking again would
    /// decode it again for ever.
    fn window_for(&mut self, secs: f64) -> Option<Window> {
        let span = self.frame_duration * (BACKWARD_WINDOW_FRAMES as f64 - 1.0);
        let would_cover = |end: f64| secs <= end + self.frame_duration * 0.5 && secs >= end - span;
        loop {
            if let Some((end, w)) = self.ahead.take() {
                if w.covers(secs, self.frame_duration) {
                    return Some(w);
                }
                let _ = end;
            }
            match self.pending {
                // Decoding now and it will answer: wait for it.
                Some(end) if would_cover(end) => {}
                // Decoding, but not this: let it finish, then ask.
                Some(_) => {}
                None => {
                    self.requests.send(secs).ok()?;
                    self.pending = Some(secs);
                }
            }
            let (end, w) = self.windows.recv().ok()?;
            self.fills += 1;
            if self.pending == Some(end) {
                self.pending = None;
            }
            if end == secs {
                return Some(w);
            }
            self.ahead = Some((end, w));
        }
    }

    /// Start decoding the window for `secs` before it is asked for: a
    /// reversed clip about to come up, so its first frame does not wait
    /// for a seek and a window's decode. Nothing when a window has it or
    /// one is decoding.
    pub fn prepare(&mut self, secs: f64) {
        self.take_finished();
        let fd = self.frame_duration;
        if self.pending.is_some() || self.window.covers(secs, fd) || self.ahead.as_ref().is_some_and(|(_, w)| w.covers(secs, fd)) {
            return;
        }
        if self.requests.send(secs).is_ok() {
            self.pending = Some(secs);
        }
    }

    /// A window that finished decoding meanwhile, kept for when it is
    /// needed.
    fn take_finished(&mut self) {
        if let Ok((end, w)) = self.windows.try_recv() {
            self.fills += 1;
            if self.pending == Some(end) {
                self.pending = None;
            }
            self.ahead = Some((end, w));
        }
    }

    /// Ask for the window before this one, to be decoding while this one
    /// is shown — unless one is decoding or decoded already, or this one
    /// starts the file.
    fn ask_ahead(&mut self) {
        self.take_finished();
        if self.pending.is_some() || self.ahead.is_some() {
            return;
        }
        let Some((first, _)) = self.window.frames.first() else { return };
        if *first <= self.frame_duration * 0.5 {
            return;
        }
        let end = first - self.frame_duration * 0.5;
        if self.requests.send(end).is_ok() {
            self.pending = Some(end);
        }
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

#[cfg(test)]
mod backward_tests {
    use super::*;

    /// #617: a file read at falling times, a frame at a time from its
    /// last to its first, gives every frame in turn, the file's last ones
    /// included, from one seek per window rather than one per frame.
    #[test]
    fn a_file_reads_backward_frame_by_frame() {
        let path = std::env::temp_dir().join(format!("klipp-backward-{}.mp4", std::process::id()));
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=25:duration=3"])
            // B-frames and a long GOP: frames out of decode order, and a
            // keyframe well before most times asked for.
            .args(["-c:v", "mpeg4", "-bf", "2", "-g", "50"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("skipped: ffmpeg could not write the test clip");
            return;
        }
        let mut r = BackwardFrameReader::open_linear(&path, None, klipp_core::WorkingColorspace::Rec709Linear).expect("opens");
        let fd = 1.0 / 25.0;
        let mut seen = Vec::new();
        for k in (0..75).rev() {
            let (_, w, h, pts) = r.decode_at(k as f64 * fd + fd * 0.25, Some((32, 24))).expect("a frame");
            assert_eq!((w, h), (32, 24));
            seen.push((pts / fd).round() as i64);
        }
        assert_eq!(seen, (0..75).rev().collect::<Vec<i64>>(), "every frame, last to first");
        assert!(r.fills() <= 4, "{} seeks for 75 frames", r.fills());

        // Past the file's end — a transition into a reversed clip's head
        // reaches there — is the last frame, at once, not a decode asked
        // for again and again.
        let mut past = BackwardFrameReader::open_linear(&path, None, klipp_core::WorkingColorspace::Rec709Linear).expect("opens");
        let t0 = std::time::Instant::now();
        let (_, _, _, pts) = past.decode_at(10.0, Some((32, 24))).expect("the last frame");
        assert_eq!((pts / fd).round() as i64, 74, "the file's last frame");
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "took {:?}", t0.elapsed());
        assert!(past.decode_at(10.0, Some((32, 24))).is_some() && past.fills() == 1, "and once: {} windows", past.fills());
        std::fs::remove_file(&path).ok();
    }
}

#[cfg(test)]
mod backward_bench {
    use super::*;

    /// Walk a real file backward for two seconds from its middle at its
    /// own rate, at a viewer's size, and say what a frame costs and the
    /// worst one (the window refill). `KLIPP_REVERSE_BENCH=<path>`.
    #[test]
    #[ignore]
    fn walk_backward_on_a_real_file() {
        let Ok(path) = std::env::var("KLIPP_REVERSE_BENCH") else { return };
        let mut r = BackwardFrameReader::open_linear(Path::new(&path), None, klipp_core::WorkingColorspace::Rec709Linear).expect("opens");
        let fd = r.frame_duration();
        let mid = std::env::var("KLIPP_REVERSE_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
        let n = (2.0 / fd) as usize;
        let mut times = Vec::new();
        // Paced as playback paces it: a frame each frame's time.
        let begun = std::time::Instant::now();
        for k in 0..n {
            let due = begun + std::time::Duration::from_secs_f64(k as f64 * fd);
            if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(wait);
            }
            let t0 = std::time::Instant::now();
            r.decode_upright(mid - k as f64 * fd, (960, 540), 0).expect("a frame");
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        let mean = times[1..].iter().sum::<f64>() / (n - 1) as f64;
        let worst = times[1..].iter().cloned().fold(0.0, f64::max);
        let late = times[1..].iter().filter(|t| **t > fd * 1000.0).count();
        eprintln!("{path}: {n} frames back at {:.1} fps: first {:.0} ms, then mean {mean:.1} ms, worst {worst:.1} ms, {late} over a frame's time, {} windows", 1.0 / fd, times[0], r.fills());
    }
}

#[cfg(test)]
mod forward_tests {
    use super::*;

    /// A ramp, frame n a flat grey of 20 + 4n, 48 frames at 24 fps, in
    /// `codec`'s arguments: `None` without ffmpeg or the encoder.
    fn ramp(name: &str, codec: &[&str]) -> Option<std::path::PathBuf> {
        let path = std::env::temp_dir().join(format!("klipp-forward-{name}-{}.mkv", std::process::id()));
        std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=32x32:r=24:d=2,geq=lum='20+4*N':cb=128:cr=128"])
            .args(codec)
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success())
            .then_some(path)
    }

    /// Every frame of a file read forward is there, its last ones included
    /// (a decoder with frame threads holds them until the end of file);
    /// past the end there is none; and the reader seeks back to the first
    /// frame after. Intra-coded ffv1, and h264 with B-frames, frames out
    /// of decode order.
    #[test]
    fn a_file_reads_forward_to_its_last_frame_and_back() {
        for (name, codec) in [
            ("ffv1", &["-c:v", "ffv1", "-pix_fmt", "yuv420p"][..]),
            ("h264", &["-c:v", "libx264", "-bf", "2", "-g", "12", "-crf", "10", "-pix_fmt", "yuv420p"][..]),
        ] {
            let Some(path) = ramp(name, codec) else {
                eprintln!("skipped {name}: ffmpeg could not write the test clip");
                continue;
            };
            let mut r = SeekableFrameReader::open_rgba(&path).expect("opens");
            let fd = r.frame_duration();
            let grey = |r: &mut SeekableFrameReader<Vec<u8>>, k: i64| r.decode_at(k as f64 * fd).map(|(px, _, _)| px[0] as i64);
            let greys: Vec<i64> = (0..48).map(|k| grey(&mut r, k).unwrap_or_else(|| panic!("{name}: frame {k} missing"))).collect();
            assert!(greys.windows(2).all(|w| w[1] > w[0]), "{name}: every frame its own, in order: {greys:?}");
            assert_eq!(grey(&mut r, 60), None, "{name}: none past the end");
            assert_eq!(grey(&mut r, 0), Some(greys[0]), "{name}: the first frame again, after the end");
            assert_eq!(grey(&mut r, 47), Some(greys[47]), "{name}: and the last again");
            // Out of order, as a viewer warming the next frame first asks:
            // a frame or two back from where the decoder sits, past ones
            // read forward and not kept, is that frame, never a later one.
            let mut r = SeekableFrameReader::open_rgba(&path).expect("opens");
            for k in [5, 10, 9, 8, 12, 11, 20, 18, 19, 30, 29, 31, 28] {
                assert_eq!(grey(&mut r, k), Some(greys[k as usize]), "{name}: frame {k} out of order");
            }
            std::fs::remove_file(&path).ok();
        }
    }
}

#[cfg(test)]
mod matrix_tests {
    use super::*;

    /// One flat sRGB colour, written as `pix_fmt` Y'CbCr with `matrix`
    /// and `range` and tagged so: `None` without ffmpeg.
    fn flat(name: &str, rgb: [u8; 3], pix_fmt: &str, matrix: &str, range: &str) -> Option<std::path::PathBuf> {
        let path = std::env::temp_dir().join(format!("klipp-matrix-{name}-{}.mkv", std::process::id()));
        let colour = format!("color=c=0x{:02X}{:02X}{:02X}:s=32x32:r=24:d=0.25", rgb[0], rgb[1], rgb[2]);
        let full = if range == "pc" { "full" } else { "limited" };
        let vf = format!(
            "format=rgb24,scale=out_color_matrix={matrix}:out_range={full},format={pix_fmt},\
             setparams=color_primaries=bt709:color_trc=bt709"
        );
        std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &colour, "-vf", &vf])
            .args(["-colorspace", matrix, "-color_range", range, "-c:v", "ffv1"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success())
            .then_some(path)
    }

    /// The centre pixel of `path`'s first frame as ffmpeg decodes it,
    /// told the matrix and range, to 16-bit RGB (its 10-bit to 8-bit
    /// route rounds a further two levels off) and rounded to 8 bits.
    fn ffmpeg_rgb(path: &Path, matrix: &str, range: &str) -> [u8; 3] {
        let out = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", &format!("scale=in_color_matrix={matrix}:in_range={range},format=rgb48le"), "-f", "rawvideo", "-"])
            .output()
            .expect("ffmpeg ran a moment ago");
        let mid = (16 * 32 + 16) * 6;
        std::array::from_fn(|c| {
            let v = u16::from_le_bytes([out.stdout[mid + c * 2], out.stdout[mid + c * 2 + 1]]);
            (v as f32 / 257.0).round() as u8
        })
    }

    /// A neighbour frame is decoded with the file's own matrix and range,
    /// at its own depth: it comes back the colour ffmpeg decodes, told
    /// the file's matrix and range, for BT.709 (limited, and full range
    /// at 10 bits 4:2:0), BT.601 full range and BT.2020, at 8, 10 and 12
    /// bits. Through 8-bit swscale with its
    /// default BT.601 matrix a BT.709 red came back 13 levels off
    /// (#1995).
    #[test]
    fn a_neighbour_decodes_with_the_files_matrix_and_range() {
        let red = [200u8, 40, 40];
        for (name, pix_fmt, matrix, range) in [
            ("709-tv-444", "yuv444p", "bt709", "tv"),
            ("709-tv-420-10", "yuv420p10le", "bt709", "tv"),
            ("709-pc-420-10", "yuv420p10le", "bt709", "pc"),
            ("601-pc-444", "yuv444p", "bt470bg", "pc"),
            ("2020-tv-444-12", "yuv444p12le", "bt2020nc", "tv"),
        ] {
            let Some(path) = flat(name, red, pix_fmt, matrix, range) else {
                eprintln!("skipped {name}: ffmpeg could not write the test clip");
                continue;
            };
            let tagged = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_range", "-of", "csv=p=0"])
                .arg(&path)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
            if let Ok(tagged) = tagged {
                assert_eq!(tagged, range, "{name}: the file was written {tagged}, not {range}");
            }
            let want = ffmpeg_rgb(&path, matrix, range);
            let mut r = SeekableFrameReader::open_linear(&path, None, klipp_core::WorkingColorspace::Rec709Linear).expect("opens");
            let (px, w, h) = r.decode_at(0.0).expect("a frame");
            let mid = ((h / 2 * w + w / 2) * 4) as usize;
            let got: Vec<u8> = px[mid..mid + 3]
                .iter()
                .map(|&v| (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8)
                .collect();
            for (c, (g, want)) in got.iter().zip(want).enumerate() {
                assert!(g.abs_diff(want) <= 1, "{name}: channel {c} is {g}, ffmpeg decodes {want} ({got:?})");
            }
            std::fs::remove_file(&path).ok();
        }
    }

    /// An OFX plug-in's neighbour frame (#2013) is decoded the same way:
    /// the colour ffmpeg decodes, told the file's matrix and range, within
    /// a level, for BT.709, BT.601 full range and BT.2020, at 8, 10 and 12
    /// bits. Control: plain swscale, which the reader fell back to, takes
    /// BT.601 and is many levels off on BT.709.
    #[test]
    fn a_plug_ins_neighbour_decodes_with_the_files_matrix_and_range() {
        let red = [200u8, 40, 40];
        for (name, pix_fmt, matrix, range) in [
            ("ofx-709-tv-444", "yuv444p", "bt709", "tv"),
            ("ofx-709-tv-420-10", "yuv420p10le", "bt709", "tv"),
            ("ofx-601-pc-444", "yuv444p", "bt470bg", "pc"),
            ("ofx-2020-tv-444-12", "yuv444p12le", "bt2020nc", "tv"),
        ] {
            let Some(path) = flat(name, red, pix_fmt, matrix, range) else {
                eprintln!("skipped {name}: ffmpeg could not write the test clip");
                continue;
            };
            let want = ffmpeg_rgb(&path, matrix, range);
            for sized in [false, true] {
                let mut r = if sized { SeekableFrameReader::open_colour_managed_sized(&path, None, 32, 32) } else { SeekableFrameReader::open_colour_managed(&path, None) }.expect("opens");
                let (px, w, h) = r.decode_at(0.0).expect("a frame");
                let mid = ((h / 2 * w + w / 2) * 4) as usize;
                for (c, (g, want)) in px[mid..mid + 3].iter().zip(want).enumerate() {
                    assert!(g.abs_diff(want) <= 1, "{name} (sized {sized}): channel {c} is {g}, ffmpeg decodes {want} ({:?})", &px[mid..mid + 3]);
                }
            }
            if name == "ofx-709-tv-444" {
                let mut plain = SeekableFrameReader::open_rgba(&path).expect("opens");
                let (px, w, h) = plain.decode_at(0.0).expect("a frame");
                let mid = ((h / 2 * w + w / 2) * 4) as usize;
                let off = px[mid..mid + 3].iter().zip(want).map(|(g, w)| g.abs_diff(w)).max().unwrap();
                assert!(off > 5, "swscale's BT.601 is {off} levels off, which the control expects to see");
            }
            std::fs::remove_file(&path).ok();
        }
    }
}
