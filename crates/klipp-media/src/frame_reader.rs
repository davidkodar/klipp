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

/// Turn a decoded frame into sRGB RGBA8 at the given size.
pub type ConvertFn =
    Box<dyn FnMut(&ffmpeg_next::frame::Video, u32, u32) -> Result<Vec<u8>, String> + Send>;

/// How many decoded frames to keep. A plugin reaching a couple of
/// frames either side of the one being rendered finds them here rather
/// than decoding twice.
const CACHE_FRAMES: usize = 8;

pub struct SeekableFrameReader {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Video,
    stream_index: usize,
    frame: ffmpeg_next::frame::Video,
    /// Where the decoder is, in seconds. Negative before the first read.
    current_pts: f64,
    frame_duration: f64,
    cache: Vec<(f64, Vec<u8>, u32, u32)>,
    convert: ConvertFn,
}

impl SeekableFrameReader {
    /// Open `path` and decode through `convert`.
    pub fn open(path: &Path, convert: ConvertFn) -> Option<Self> {
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
        F: FnOnce(&ffmpeg_next::format::context::Input, usize) -> ConvertFn,
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

    /// Open `path` converting with plain swscale to RGBA.
    ///
    /// For callers with no colour rules of their own. A scene-referred
    /// source (PQ, HLG, camera log) comes back as swscale renders it,
    /// which is flat for log and washed out for PQ — see the issue on
    /// the two paths disagreeing about colour.
    pub fn open_rgba(path: &Path) -> Option<Self> {
        Self::open(path, Box::new(swscale_rgba))
    }

    /// The length of one frame, in seconds.
    pub fn frame_duration(&self) -> f64 {
        self.frame_duration
    }

    /// The frame at `seek_secs`, or `None` past the end of the file.
    pub fn decode_at(&mut self, seek_secs: f64) -> Option<(Vec<u8>, u32, u32)> {
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
