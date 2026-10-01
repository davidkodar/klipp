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
        Self::open_with(path, move |format_ctx, stream_index| linear_convert(format_ctx, stream_index, log_format, working))
    }
}

/// The conversion [`SeekableFrameReader::open_linear`] and
/// [`BackwardFrameReader::open_linear`] decode through.
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
        // Keep a frame; `true` once past the time asked for.
        let keep = |pts: f64, frame: ffmpeg_next::frame::Video, frames: &mut Vec<(f64, ffmpeg_next::frame::Video)>| -> bool {
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
        let format_ctx = ffmpeg_next::format::input(path).ok()?;
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
    /// does, the one decoding when it will, else one decoded now.
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
        std::fs::remove_file(&path).ok();
        assert_eq!(seen, (0..75).rev().collect::<Vec<i64>>(), "every frame, last to first");
        assert!(r.fills() <= 4, "{} seeks for 75 frames", r.fills());
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
