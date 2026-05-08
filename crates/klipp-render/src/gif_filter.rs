//! GIF two-pass palette filter graph.
//!
//! Animated GIFs are limited to a 256-colour palette. Producing a
//! quality GIF requires two filter stages:
//!
//! 1. **`palettegen`** analyses every frame and produces a single
//!    optimal 256-colour palette tuned to the actual content.
//! 2. **`paletteuse`** applies that palette to each frame with a
//!    dithering algorithm to mask the colour-reduction banding.
//!
//! ffmpeg's canonical CLI form does this in a single `filter_complex`:
//!
//! ```text
//! [0:v]split=2[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse
//! ```
//!
//! `split=2` duplicates the input. The first branch runs through
//! `palettegen` (which buffers all frames internally to compute the
//! global palette). The second branch waits inside `paletteuse` until
//! the palette arrives, then emits palettized PAL8 frames.
//!
//! We set up the same graph in-process via [`ffmpeg_next::filter::Graph`].
//! Frames pushed via [`GifFilter::push_frame`] return `Vec<Frame>` —
//! empty until end-of-stream because palettegen with
//! `stats_mode=full` only emits its palette after seeing every input
//! frame. [`GifFilter::finish`] signals EOS and drains the queue,
//! returning every paletted frame in one batch.
//!
//! Memory cost: the filter graph buffers all input frames internally
//! between split and paletteuse. For a typical 5–10s 480p GIF
//! (~75–150 frames) this is ~120–240 MB, well within bounds. Long
//! 1080p GIFs (>30s) are a pathological case that the GIF format
//! itself can't really serve well.

use crate::error::RenderError;

/// Owns the palettegen + paletteuse filter graph. One per export.
pub struct GifFilter {
    graph: ffmpeg_next::filter::Graph,
    width: u32,
    height: u32,
    frame_pts: i64,
}

impl GifFilter {
    /// Build the filter graph. `time_base_den` is the GIF's intended
    /// frame-rate denominator (e.g. 15 for 15 fps); palettegen uses
    /// it to position the global palette in time.
    pub fn new(width: u32, height: u32, time_base_den: u32) -> Result<Self, RenderError> {
        let mut graph = ffmpeg_next::filter::Graph::new();

        let buffer_args = format!(
            "video_size={width}x{height}:pix_fmt=rgba:time_base=1/{time_base_den}:pixel_aspect=1/1"
        );

        let buffer_filter = ffmpeg_next::filter::find("buffer")
            .ok_or_else(|| RenderError::Encode("filter `buffer` not registered".into()))?;
        let sink_filter = ffmpeg_next::filter::find("buffersink")
            .ok_or_else(|| RenderError::Encode("filter `buffersink` not registered".into()))?;

        graph
            .add(&buffer_filter, "in", &buffer_args)
            .map_err(|e| RenderError::Encode(format!("graph add buffer: {e}")))?;
        graph
            .add(&sink_filter, "out", "")
            .map_err(|e| RenderError::Encode(format!("graph add buffersink: {e}")))?;

        // Constrain the sink to PAL8 — paletteuse outputs PAL8 frames
        // already, but pinning it here also gives ffmpeg permission
        // to insert format-conversion auto-filters if anything in
        // the graph deviates.
        if let Some(mut out) = graph.get("out") {
            out.set_pixel_format(ffmpeg_next::format::Pixel::PAL8);
        }

        // The filter spec: split the input into two streams, run
        // palettegen on one and paletteuse on the other (taking the
        // palette as its second input). The full-stats palette mode
        // analyses every frame for the most accurate global palette.
        // Sierra2-4A dithering masks colour banding without adding
        // noticeable noise — best-quality default per ffmpeg docs.
        let filter_spec = "split=2[a][b];\
                           [a]palettegen=stats_mode=full:max_colors=256[p];\
                           [b][p]paletteuse=dither=sierra2_4a";

        graph
            .output("in", 0)
            .map_err(|e| RenderError::Encode(format!("graph output(in): {e}")))?
            .input("out", 0)
            .map_err(|e| RenderError::Encode(format!("graph input(out): {e}")))?
            .parse(filter_spec)
            .map_err(|e| RenderError::Encode(format!("graph parse: {e}")))?;

        graph
            .validate()
            .map_err(|e| RenderError::Encode(format!("graph validate: {e}")))?;

        Ok(Self {
            graph,
            width,
            height,
            frame_pts: 0,
        })
    }

    /// Push one RGBA frame into the filter graph. Returns any output
    /// frames that became available — typically empty until
    /// [`GifFilter::finish`] signals end-of-stream, because palettegen
    /// in `stats_mode=full` only emits its palette after seeing every
    /// input frame.
    pub fn push_frame(
        &mut self,
        rgba: &[u8],
    ) -> Result<Vec<ffmpeg_next::frame::Video>, RenderError> {
        let mut frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::RGBA,
            self.width,
            self.height,
        );

        // Copy RGBA bytes into the frame's plane 0. ffmpeg may add
        // row padding, so we copy row-by-row using the actual stride.
        let stride = frame.stride(0);
        let row_bytes = (self.width * 4) as usize;
        debug_assert!(rgba.len() >= row_bytes * self.height as usize);
        {
            let dst = frame.data_mut(0);
            for y in 0..self.height as usize {
                let src_off = y * row_bytes;
                let dst_off = y * stride;
                dst[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&rgba[src_off..src_off + row_bytes]);
            }
        }

        frame.set_pts(Some(self.frame_pts));
        self.frame_pts += 1;

        let mut source = self
            .graph
            .get("in")
            .ok_or_else(|| RenderError::Encode("graph filter `in` missing".into()))?;
        source
            .source()
            .add(&frame)
            .map_err(|e| RenderError::Encode(format!("filter source.add: {e}")))?;

        self.drain()
    }

    /// Signal end-of-stream and drain every remaining output frame.
    /// This is when palettegen finally emits its global palette and
    /// paletteuse emits all the buffered video frames.
    pub fn finish(&mut self) -> Result<Vec<ffmpeg_next::frame::Video>, RenderError> {
        let mut source = self
            .graph
            .get("in")
            .ok_or_else(|| RenderError::Encode("graph filter `in` missing".into()))?;
        source
            .source()
            .flush()
            .map_err(|e| RenderError::Encode(format!("filter source.flush: {e}")))?;

        self.drain()
    }

    /// Pull every available frame from the buffersink. EAGAIN /
    /// EOF terminate the loop.
    fn drain(&mut self) -> Result<Vec<ffmpeg_next::frame::Video>, RenderError> {
        let mut out = Vec::new();
        loop {
            let mut frame = ffmpeg_next::frame::Video::empty();
            let mut sink = self
                .graph
                .get("out")
                .ok_or_else(|| RenderError::Encode("graph filter `out` missing".into()))?;
            match sink.sink().frame(&mut frame) {
                Ok(()) => out.push(frame),
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == ffmpeg_next::ffi::EAGAIN =>
                {
                    break;
                }
                Err(ffmpeg_next::Error::Eof) => break,
                Err(e) => {
                    return Err(RenderError::Encode(format!("filter sink.frame: {e}")))
                }
            }
        }
        Ok(out)
    }
}
