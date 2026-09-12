//! FFmpeg-based scope renderer using libavfilter.
//!
//! Flag-gated alternative to the in-tree GPU shader path. Mirrors the
//! standalone `scope_test` prototype at `~/Documents/claude/tools/scope_test`:
//! the scope chain takes a frame in, runs a libavfilter
//! `waveform` / `vectorscope` / `histogram` filter graph, and emits a
//! 1024×512 RGBA u8 frame the caller writes into
//! `FrameBuffer::scope_texture` exactly the same way as the GPU path.
//!
//! Two input formats are supported:
//!
//! * [`InputFormat::U8Srgb`] — the simple path. Caller supplies a
//!   display-encoded sRGB RGBA u8 buffer (the final composited frame).
//!   Internally the graph picks up `format=gbrp,…` to force planar G/B/R
//!   before the waveform filter so per-channel parade colouring works.
//!
//! * [`InputFormat::F32Linear10`] — the precision path. Caller supplies a
//!   linear-light BT.709 RGBA f32 buffer (the engine's `result_linear`).
//!   The wrapper applies the sRGB OETF in float, quantises to 10-bit code
//!   values, packs into planar `gbrp10le`, and feeds the filter graph
//!   directly. Pro-scope readings in display-encoded code values with
//!   10-bit headroom, end-to-end.

use std::collections::HashMap;

use ffmpeg_next::{filter, frame, util::format::Pixel};
use klipp_color::scopes::ScopeBasis;
use klipp_core::{ScopeFfmpegSettings, ScopeQuality};

type Result<T> = std::result::Result<T, String>;

/// Output dimensions. Matches the engine's existing `scope_w / scope_h`
/// constants so the dock display path doesn't need to change.
pub const SCOPE_OUT_W: u32 = 1024;
pub const SCOPE_OUT_H: u32 = 512;

/// The CIE chromaticity scope's place in `active_scope_modes`.
///
/// Not a filter mode — libavfilter has no gamut scope, and this one is drawn
/// in the UI. What the byte asks the engine for is
/// [`crate::texture::FrameBuffer::cie_samples`]: the picture in the
/// project's working space, linear, before the display transform. Sits above
/// the four filter modes (0..=3) so nothing tries to build a graph for it.
pub const CIE_SAMPLES_MODE: u8 = 4;

/// Longest edge of the sample grid the CIE scope measures.
///
/// The scope plots one dot per sample and the diagram is a few hundred
/// pixels across, so more samples than this buy nothing but memory: a 4K
/// frame reduces to 384x216 RGB f32, under a megabyte, at the ≤10 Hz the
/// scope path already runs at.
pub const CIE_SAMPLES_MAX_EDGE: u32 = 384;

/// Which pixel format the caller is supplying to [`FfmpegScope::render`].
/// Drives the buffer-source pix_fmt + the leading format-conversion clause
/// in the filter spec, and selects which `process_*` method runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    /// 8-bit display-encoded sRGB RGBA. Existing engine path.
    U8Srgb,
    /// Linear-light BT.709 RGBA f32. Wrapper applies sRGB OETF + 10-bit
    /// quantise + planar split internally; filter graph receives
    /// `gbrp10le`. Used when the engine taps `result_linear` before
    /// `display_encode()`.
    F32Linear10,
    /// Linear-light RGBA f32 that the wrapper encodes and converts to
    /// full-range `yuv444p` **itself**, in the project's working space
    /// (see [`FfmpegScope::render_linear_f32`]).
    ///
    /// Used by the scopes that measure Y′CbCr rather than R′G′B′: the
    /// vectorscope, and the waveform and parade in luma-only mode.
    /// Letting `format=yuv444p,` do it instead handed the job to
    /// libswscale, which converts as Rec.601 limited-range no matter
    /// what the picture is — so the trace was drawn in a matrix the
    /// project does not use, at 224/255 of the scale the graticule is
    /// drawn at.
    F32Yuv444,
}

pub struct FfmpegScope {
    graph: Option<filter::Graph>,
    current_mode: u8,
    current_src_w: u32,
    current_src_h: u32,
    current_format: InputFormat,
    current_settings: ScopeFfmpegSettings,
    pts: i64,
}

impl FfmpegScope {
    pub fn new() -> Self {
        let _ = ffmpeg_next::init();
        Self {
            graph: None,
            current_mode: u8::MAX,
            current_src_w: 0,
            current_src_h: 0,
            current_format: InputFormat::U8Srgb,
            current_settings: ScopeFfmpegSettings::default(),
            pts: 0,
        }
    }

    /// Render one scope frame from an 8-bit display-encoded sRGB RGBA
    /// buffer. Returns `Ok(None)` when the filter buffered the input
    /// without emitting (rare for scope filters — treat as "no scope
    /// this tick").
    pub fn render(
        &mut self,
        mode: u8,
        src_rgba: &[u8],
        src_w: u32,
        src_h: u32,
        settings: ScopeFfmpegSettings,
    ) -> Result<Option<Vec<u8>>> {
        if src_w == 0 || src_h == 0 {
            return Ok(None);
        }
        if mode != self.current_mode
            || src_w != self.current_src_w
            || src_h != self.current_src_h
            || self.current_format != InputFormat::U8Srgb
            || self.current_settings != settings
            || self.graph.is_none()
        {
            self.rebuild(mode, src_w, src_h, InputFormat::U8Srgb, settings)?;
        }
        self.process_u8(src_rgba, src_w, src_h)
    }

    /// Render one scope frame from a linear-light RGBA f32 buffer, measured
    /// in `basis` — the project's working space.
    ///
    /// R′G′B′ scopes (waveform, parade, histogram) get sRGB OETF + 10-bit
    /// quantise + planar split as `gbrp10le`. The Y′CbCr scopes get a
    /// full-range `yuv444p` frame built here, from `basis`, so the trace
    /// is measured in the same matrix the graticule is drawn from. Same
    /// `Ok(None)` semantics as [`Self::render`].
    pub fn render_linear_f32(
        &mut self,
        mode: u8,
        src_linear_rgba: &[f32],
        src_w: u32,
        src_h: u32,
        settings: ScopeFfmpegSettings,
        basis: ScopeBasis,
    ) -> Result<Option<Vec<u8>>> {
        if src_w == 0 || src_h == 0 {
            return Ok(None);
        }
        let format = if measures_chroma(mode, &settings) {
            InputFormat::F32Yuv444
        } else {
            InputFormat::F32Linear10
        };
        if mode != self.current_mode
            || src_w != self.current_src_w
            || src_h != self.current_src_h
            || self.current_format != format
            || self.current_settings != settings
            || self.graph.is_none()
        {
            self.rebuild(mode, src_w, src_h, format, settings)?;
        }
        match format {
            InputFormat::F32Yuv444 => self.process_yuv444(src_linear_rgba, src_w, src_h, basis),
            _ => self.process_linear_f32(src_linear_rgba, src_w, src_h),
        }
    }

    fn rebuild(
        &mut self,
        mode: u8,
        src_w: u32,
        src_h: u32,
        format: InputFormat,
        settings: ScopeFfmpegSettings,
    ) -> Result<()> {
        // Drop the old graph BEFORE constructing the new one. Holding
        // both alive simultaneously can trigger SIGSEGV in
        // avfilter_graph_free if the freed graph racially touches
        // shared codec context state that the fresh graph is concurrently
        // setting up. The pts counter also resets at the end of this
        // function so the new graph starts at pts=0.
        self.graph = None;
        let mut graph = filter::Graph::new();
        let pix_fmt = match format {
            InputFormat::U8Srgb => "rgba",
            InputFormat::F32Linear10 => "gbrp10le",
            InputFormat::F32Yuv444 => "yuv444p",
        };
        let buffer_args = format!(
            "video_size={src_w}x{src_h}:pix_fmt={pix_fmt}:time_base=1/30:pixel_aspect=1/1"
        );
        let buffer = filter::find("buffer")
            .ok_or_else(|| "ffmpeg filter `buffer` not registered".to_string())?;
        let sink = filter::find("buffersink")
            .ok_or_else(|| "ffmpeg filter `buffersink` not registered".to_string())?;

        graph
            .add(&buffer, "in", &buffer_args)
            .map_err(|e| format!("graph.add buffer: {e}"))?;
        graph
            .add(&sink, "out", "")
            .map_err(|e| format!("graph.add buffersink: {e}"))?;

        let scope_spec = scope_spec_for_mode(mode, &settings, format)
            .ok_or_else(|| format!("FfmpegScope: no spec for mode {mode}"))?;

        // Prepend `format=gbrp,` for BOTH input formats so FFmpeg
        // converts to 8-bit planar G/B/R BEFORE the waveform filter
        // runs. Two reasons:
        //
        // 1. U8 path: catches the original "all-green parade" regression
        //    where the libavfilter optimiser could pick a YUV intermediate
        //    between the buffer source and the waveform filter,
        //    collapsing per-channel colouring.
        //
        // 2. F32 path: feeding `gbrp10le` directly to waveform produces
        //    wrong per-section parade colours in libavfilter 9.x —
        //    sections appear in shifted/rotated channel order
        //    (B,R,G or G,B,R instead of R,G,B) and the FLAT/ACOLOR/etc.
        //    filter modes SIGSEGV outright. Downgrading to gbrp before
        //    the filter restores the prototype's correct R,G,B parade.
        //    We lose 2 bits of precision through this conversion vs the
        //    pre-quantised 10-bit code-value pack — acceptable trade for
        //    correct visuals while a 10-bit-clean libavfilter path is
        //    investigated as a follow-up.
        // …except when the caller already built Y′CbCr planes: forcing
        // planar RGB there would undo the conversion this path exists to
        // control, and libswscale would put its own back.
        let format_prefix = if format == InputFormat::F32Yuv444 { "" } else { "format=gbrp," };
        // Quality preset drives an input downsample. Inserting it as
        // the FIRST filter (before the format conversion for U8 — yes
        // that means scale operates on RGBA u8 there, which FFmpeg
        // handles fine; the format=gbrp,... order proven in 2.1) keeps
        // the histogram cost roughly constant across resolutions.
        // Skip the scale clause when the source is already smaller
        // than the target so we don't UP-scale.
        let (q_w, q_h) = settings.quality.ffmpeg_downsample();
        let downsample_prefix = if src_w > q_w || src_h > q_h {
            format!("scale={q_w}:{q_h},")
        } else {
            String::new()
        };
        let full_spec = format!(
            "{downsample_prefix}{format_prefix}{scope_spec},scale={SCOPE_OUT_W}:{SCOPE_OUT_H},format=rgba"
        );

        graph
            .output("in", 0)
            .map_err(|e| format!("graph.output(in): {e}"))?
            .input("out", 0)
            .map_err(|e| format!("graph.input(out): {e}"))?
            .parse(&full_spec)
            .map_err(|e| format!("parse filter spec {full_spec}: {e}"))?;

        graph
            .validate()
            .map_err(|e| format!("graph.validate: {e}"))?;

        self.graph = Some(graph);
        self.current_mode = mode;
        self.current_src_w = src_w;
        self.current_src_h = src_h;
        self.current_format = format;
        self.current_settings = settings;
        self.pts = 0;
        tracing::info!(
            "FfmpegScope: rebuilt for mode={mode} fmt={:?} {src_w}x{src_h} → {SCOPE_OUT_W}x{SCOPE_OUT_H} spec={full_spec}",
            format,
        );
        Ok(())
    }

    fn process_u8(
        &mut self,
        src_rgba: &[u8],
        src_w: u32,
        src_h: u32,
    ) -> Result<Option<Vec<u8>>> {
        let mut input = frame::Video::new(Pixel::RGBA, src_w, src_h);
        input.set_pts(Some(self.pts));
        self.pts += 1;

        let stride = input.stride(0) as usize;
        let row_bytes = (src_w * 4) as usize;
        let height = src_h as usize;
        if src_rgba.len() < row_bytes * height {
            return Err(format!(
                "FfmpegScope: RGBA buffer too small: {} bytes, need {}",
                src_rgba.len(),
                row_bytes * height
            ));
        }
        {
            let dst = input.data_mut(0);
            for y in 0..height {
                let src_off = y * row_bytes;
                let dst_off = y * stride;
                dst[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&src_rgba[src_off..src_off + row_bytes]);
            }
        }

        self.feed_and_pull(input)
    }

    fn process_linear_f32(
        &mut self,
        src_linear_rgba: &[f32],
        src_w: u32,
        src_h: u32,
    ) -> Result<Option<Vec<u8>>> {
        let width = src_w as usize;
        let height = src_h as usize;
        let pixel_count = width * height;
        if src_linear_rgba.len() < pixel_count * 4 {
            return Err(format!(
                "FfmpegScope: linear f32 buffer too small: {} floats, need {}",
                src_linear_rgba.len(),
                pixel_count * 4,
            ));
        }

        let mut input = frame::Video::new(Pixel::GBRP10LE, src_w, src_h);
        input.set_pts(Some(self.pts));
        self.pts += 1;

        // Linear-BT.709 f32 RGBA → sRGB OETF → 10-bit code values, split
        // into planar G/B/R. `gbrp10le` stores each plane as little-endian
        // u16 with values in [0, 1023]; the upper 6 bits are unused.
        //
        // FFmpeg's plane order for `gbrp*` is G=0, B=1, R=2 (not R-G-B).
        // Walking the source in pixel-stride-4 order and writing to three
        // planes simultaneously keeps the inner loop cache-friendly and
        // avoids per-channel passes.
        //
        // Stride values are reported in bytes; we write u16 so step by
        // `stride/2` pixels per row in the destination plane.
        let stride_g = input.stride(0) as usize / 2;
        let stride_b = input.stride(1) as usize / 2;
        let stride_r = input.stride(2) as usize / 2;

        // Borrow each plane individually — `data_mut` returns a `&mut [u8]`
        // for the requested plane and there's no multi-plane accessor, so
        // we fill one plane at a time. The inner loop runs three times per
        // tick at 10 Hz max so the extra pass cost is negligible vs the
        // alternative (unsafe pointer juggling).
        {
            let plane = bytemuck::cast_slice_mut::<u8, u16>(input.data_mut(0));
            for y in 0..height {
                let row_in = y * width * 4;
                let row_out = y * stride_g;
                for x in 0..width {
                    let g_lin = src_linear_rgba[row_in + x * 4 + 1];
                    plane[row_out + x] = linear_to_srgb_10bit(g_lin);
                }
            }
        }
        {
            let plane = bytemuck::cast_slice_mut::<u8, u16>(input.data_mut(1));
            for y in 0..height {
                let row_in = y * width * 4;
                let row_out = y * stride_b;
                for x in 0..width {
                    let b_lin = src_linear_rgba[row_in + x * 4 + 2];
                    plane[row_out + x] = linear_to_srgb_10bit(b_lin);
                }
            }
        }
        {
            let plane = bytemuck::cast_slice_mut::<u8, u16>(input.data_mut(2));
            for y in 0..height {
                let row_in = y * width * 4;
                let row_out = y * stride_r;
                for x in 0..width {
                    let r_lin = src_linear_rgba[row_in + x * 4];
                    plane[row_out + x] = linear_to_srgb_10bit(r_lin);
                }
            }
        }

        self.feed_and_pull(input)
    }

    /// Linear working-space RGBA f32 → full-range 8-bit `yuv444p`, in
    /// `basis`.
    ///
    /// Y′CbCr is built on the *encoded* R′G′B′ — the prime is the whole
    /// point — so the sRGB OETF goes on first, exactly as for the R′G′B′
    /// scopes, and then the working space's own luminance row and
    /// colour-difference pair.
    ///
    /// Full range, not limited: the vectorscope plots raw code values and
    /// the UI's graticule is drawn in full-scale Cb/Cr, so a limited-range
    /// frame would draw every colour at 224/255 of its radius — inside its
    /// own target box. Code 128 is the neutral axis; `127.5 + C * 255`
    /// puts Cb = ±0.5 on the edges.
    fn process_yuv444(
        &mut self,
        src_linear_rgba: &[f32],
        src_w: u32,
        src_h: u32,
        basis: ScopeBasis,
    ) -> Result<Option<Vec<u8>>> {
        let (width, height) = (src_w as usize, src_h as usize);
        if src_linear_rgba.len() < width * height * 4 {
            return Err(format!(
                "FfmpegScope: linear f32 buffer too small: {} floats, need {}",
                src_linear_rgba.len(),
                width * height * 4,
            ));
        }

        let mut input = frame::Video::new(Pixel::YUV444P, src_w, src_h);
        input.set_pts(Some(self.pts));
        self.pts += 1;

        let strides = [
            input.stride(0) as usize,
            input.stride(1) as usize,
            input.stride(2) as usize,
        ];
        let code = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        let chroma_code = |c: f32| (127.5 + c * 255.0).round().clamp(0.0, 255.0) as u8;

        for plane in 0..3 {
            let stride = strides[plane];
            let dst = input.data_mut(plane);
            for y in 0..height {
                let row_in = y * width * 4;
                let row_out = y * stride;
                for x in 0..width {
                    let i = row_in + x * 4;
                    let rgb = [
                        linear_to_srgb(src_linear_rgba[i]),
                        linear_to_srgb(src_linear_rgba[i + 1]),
                        linear_to_srgb(src_linear_rgba[i + 2]),
                    ];
                    dst[row_out + x] = match plane {
                        0 => code(basis.luma(rgb)),
                        1 => chroma_code(basis.chroma(rgb).0),
                        _ => chroma_code(basis.chroma(rgb).1),
                    };
                }
            }
        }

        self.feed_and_pull(input)
    }

    fn feed_and_pull(&mut self, input: frame::Video) -> Result<Option<Vec<u8>>> {
        let graph = self
            .graph
            .as_mut()
            .ok_or_else(|| "filter graph not initialised".to_string())?;

        let mut source = graph
            .get("in")
            .ok_or_else(|| "filter `in` missing".to_string())?;
        source
            .source()
            .add(&input)
            .map_err(|e| format!("buffer src .add: {e}"))?;

        let mut output = frame::Video::empty();
        let mut sink = graph
            .get("out")
            .ok_or_else(|| "filter `out` missing".to_string())?;
        match sink.sink().frame(&mut output) {
            Ok(()) => {
                let w = output.width();
                let h = output.height();
                let stride = output.stride(0) as usize;
                let row_bytes = (w * 4) as usize;
                let height = h as usize;
                let mut data = Vec::with_capacity(row_bytes * height);
                let src = output.data(0);
                for y in 0..height {
                    let row_start = y * stride;
                    data.extend_from_slice(&src[row_start..row_start + row_bytes]);
                }
                Ok(Some(data))
            }
            Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::ffi::EAGAIN => {
                Ok(None)
            }
            Err(ffmpeg_next::Error::Eof) => Ok(None),
            Err(e) => Err(format!("filter sink frame: {e}")),
        }
    }
}

impl Default for FfmpegScope {
    fn default() -> Self { Self::new() }
}

/// Owns one [`FfmpegScope`] per active scope mode. Lets us drive multiple
/// scopes in parallel per tick (main dock + floating multi-scope window
/// panes can each request their own mode) without rebuilding the filter
/// graph on every mode swap.
///
/// Backed by a `HashMap` keyed on the mode byte (0..=3). Instances are
/// lazily created on first request for a given mode and held for the
/// lifetime of the bank — there are only 4 modes total, each filter
/// graph is a few hundred KB, so eviction isn't worth the complexity.
pub struct FfmpegScopeBank {
    instances: HashMap<u8, FfmpegScope>,
}

impl FfmpegScopeBank {
    pub fn new() -> Self {
        Self { instances: HashMap::new() }
    }

    /// Render one mode from a linear-light BT.709 RGBA f32 buffer. The
    /// per-mode [`FfmpegScope`] is created on first call. Returns `None`
    /// on filter buffering or rebuild failure (caller treats as "no
    /// scope this tick"). `settings` change → filter graph rebuild on
    /// next tick.
    pub fn render_linear(
        &mut self,
        mode: u8,
        linear_rgba: &[f32],
        src_w: u32,
        src_h: u32,
        settings: ScopeFfmpegSettings,
        basis: ScopeBasis,
    ) -> Result<Option<Vec<u8>>> {
        let scope = self
            .instances
            .entry(mode)
            .or_insert_with(FfmpegScope::new);
        scope.render_linear_f32(mode, linear_rgba, src_w, src_h, settings, basis)
    }

    /// Render one mode from a display-encoded sRGB u8 RGBA buffer.
    /// Kept for callers that don't have a linear-light buffer in hand
    /// (e.g. raw u8-only paths during diagnostic comparisons). Engine's
    /// production path uses [`Self::render_linear`].
    #[allow(dead_code)]
    pub fn render_u8(
        &mut self,
        mode: u8,
        rgba: &[u8],
        src_w: u32,
        src_h: u32,
        settings: ScopeFfmpegSettings,
    ) -> Result<Option<Vec<u8>>> {
        let scope = self
            .instances
            .entry(mode)
            .or_insert_with(FfmpegScope::new);
        scope.render(mode, rgba, src_w, src_h, settings)
    }
}

impl Default for FfmpegScopeBank {
    fn default() -> Self { Self::new() }
}

/// Linear-BT.709 → sRGB OETF → 10-bit code value `[0, 1023]`.
///
/// Pro scopes display values in the display-encoded space (the colorist
/// thinks in "IRE" or "code values", not linear light). Klipp's working
/// space is linear-BT.709, so we apply the sRGB OETF here. For a future
/// per-output ODT picker (Rec.709 / DCI-P3 / Rec.2020) this is the hook
/// point — replace the constant `2.4` curve with a per-engine choice.
#[inline]
fn linear_to_srgb_10bit(v: f32) -> u16 {
    (linear_to_srgb(v) * 1023.0 + 0.5) as u16
}

/// The sRGB OETF, in float. The scopes read encoded values, so this is
/// what "the picture" means to every one of them.
fn linear_to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Whether this scope measures Y′CbCr rather than R′G′B′, and so needs the
/// conversion done in the project's basis instead of by libswscale.
fn measures_chroma(mode: u8, settings: &ScopeFfmpegSettings) -> bool {
    match mode {
        // Waveform and parade, but only when showing a single luma trace.
        0 | 1 => settings.luma_only,
        // The vectorscope is nothing but chroma.
        2 => true,
        _ => false,
    }
}

/// Map the UI scope-mode byte → libavfilter spec string (WITHOUT the
/// leading format-conversion clause; [`FfmpegScope::rebuild`] inserts
/// `format=gbrp,` for the 8-bit path and skips it for the 10-bit path
/// where the input is already planar).
///
/// `graticule=none` everywhere — Klipp's dock draws its own IRE axis
/// labels + gridlines on top of the texture.
///
/// All four modes read from their corresponding per-mode sub-struct in
/// `settings`. `quality` is global (perf preset) and drives the input
/// downsample (handled in [`FfmpegScope::rebuild`]) + the fallback
/// intensity when a per-mode `intensity_override` is negative.
fn scope_spec_for_mode(
    mode: u8,
    settings: &ScopeFfmpegSettings,
    format: InputFormat,
) -> Option<String> {
    // The Y'CbCr scopes need a YUV frame. When the caller built one in the
    // project's basis it is already in hand; otherwise libswscale still has
    // to make one, and its Rec.601 limited-range guess is all that path has.
    let to_yuv = if format == InputFormat::F32Yuv444 { "" } else { "format=yuv444p," };
    /// Pick per-mode override or the quality tier default.
    fn intensity_for(override_val: f32, quality: ScopeQuality) -> f32 {
        if override_val >= 0.0 { override_val } else { quality.ffmpeg_intensity() }
    }
    match mode {
        // 0 = Waveform (R/G/B stacked overlay, or single Y in luma mode)
        0 => {
            let s = &settings.waveform;
            let filter = s.filter.keyword();
            let intensity = intensity_for(s.intensity_override, settings.quality);
            let envelope = s.envelope.ffmpeg_value();
            if settings.luma_only {
                // Switch input to YUV and ask for the Y plane only.
                // `format=yuv444p,` rides on top of whatever format
                // prefix the [`rebuild`] caller prepended, so the
                // libavfilter optimiser still has a deterministic
                // path into the waveform filter.
                Some(format!(
                    "{to_yuv}waveform=mode=column:filter={filter}:intensity={intensity:.3}:display=stack:components=1:envelope={envelope}:graticule=none"
                ))
            } else {
                let components = s.channels.to_components();
                Some(format!(
                    "waveform=mode=column:filter={filter}:intensity={intensity:.3}:display=stack:components={components}:envelope={envelope}:graticule=none"
                ))
            }
        }
        // 1 = Parade (R/G/B side by side, or single Y in luma mode)
        1 => {
            let s = &settings.parade;
            let filter = s.filter.keyword();
            let intensity = intensity_for(s.intensity_override, settings.quality);
            let envelope = s.envelope.ffmpeg_value();
            if settings.luma_only {
                Some(format!(
                    "{to_yuv}waveform=mode=column:filter={filter}:intensity={intensity:.3}:display=parade:components=1:envelope={envelope}:graticule=none"
                ))
            } else {
                let components = s.channels.to_components();
                Some(format!(
                    "waveform=mode=column:filter={filter}:intensity={intensity:.3}:display=parade:components={components}:envelope={envelope}:graticule=none"
                ))
            }
        }
        // 2 = Vectorscope — plots U vs V chroma. Explicit `format=yuv444p,`
        // forces the gbrp → YUV conversion to happen via libswscale (Rec.601
        // by default) BEFORE the vectorscope filter sees the frame, instead
        // of relying on libavfilter's internal auto-negotiation. The
        // auto-path picked a degenerate format on some inputs and made
        // every pixel land on the U=V diagonal.
        //
        // Klipp's main-dock vectorscope overlay (in widgets/scopes.rs)
        // draws the saturation ring + R/Mg/B/Cy/G/Yl target dots + the
        // skin-tone I-line + the text labels, so we ask FFmpeg for the
        // raw chroma scatter only (`graticule=none`). The dropdown in the
        // popover still lets the user re-enable FFmpeg's graticule for
        // diagnostic comparison.
        2 => {
            let v = &settings.vectorscope;
            let m = v.mode.keyword();
            let g = v.graticule.keyword();
            let bg = v.bgopacity.clamp(0.0, 1.0);
            let lo = v.lthreshold.clamp(0.0, 1.0);
            let hi = v.hthreshold.clamp(0.0, 1.0);
            Some(format!(
                "{to_yuv}vectorscope=mode={m}:graticule={g}:bgopacity={bg:.3}:lthreshold={lo:.3}:hthreshold={hi:.3}"
            ))
        }
        // 3 = Histogram (R/G/B configurable display + levels mode).
        // `colors_mode=coloronblack` paints each pane's bars in the
        // channel's natural colour (R=red, G=green, B=blue) — FFmpeg's
        // default `whiteonblack` left bars white regardless of channel.
        3 => {
            let h = &settings.histogram;
            let display = h.display_mode.keyword();
            let levels = h.levels.keyword();
            let fg = h.fgopacity.clamp(0.0, 1.0);
            let bg = h.bgopacity.clamp(0.0, 1.0);
            Some(format!(
                "histogram=display_mode={display}:levels_mode={levels}:fgopacity={fg:.3}:bgopacity={bg:.3}:colors_mode=coloronblack"
            ))
        }
        _ => None,
    }
}
