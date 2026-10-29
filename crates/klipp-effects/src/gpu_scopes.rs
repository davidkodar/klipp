//! Waveform, parade, vectorscope and histogram drawn on the GPU.
//!
//! A scope is a density plot: for every column of the picture, how many
//! samples sit at each level — or, for the vectorscope, how many sit at
//! each chroma, and for the histogram, how many sit at each level across
//! the whole picture. Drawing one well is two separate jobs, and this
//! module keeps them apart.
//!
//! **Measuring** ([`ScopeRenderer::render`], the accumulate pass) reads
//! every sample of the frame — or every second or fourth, by quality —
//! encodes it for the transfer the picture is delivered in, and splats it
//! into a bin grid one column per pixel of the widest trace drawn (at
//! most [`MAX_BIN_COLS`]) by [`BIN_ROWS`] levels. The
//! levels are 10-bit, so the trace has the vertical resolution a 10-bit
//! delivery has, and a sample is split between its two nearest levels and
//! its two nearest columns by distance, which is what keeps a slow ramp
//! from breaking into steps.
//!
//! **Drawing** (the shade pass) turns the bins into pixels at whatever
//! size the panel is. Each output pixel sums the bins it covers, so a
//! small panel is a true downsample rather than a resized picture, and
//! the count becomes brightness through `1 − e^(−density · gain)`: a lone
//! sample is a faint dot, a few stacked samples a clear line, a flat area
//! a saturated core. That curve — dim for sparse, bright for dense, never
//! clipping into a blob — is the look of a phosphor scope, and of the
//! software scopes that imitate one. A separable glow adds the light that
//! bleeds off a dense trace. Nothing here is a fixed raster resized
//! after the fact.
//!
//! The picture arrives as the renderer's linear working-space frame, or as
//! the display bytes for a working space whose display transform has no
//! GPU form; either way the levels shown are the delivery's, not the
//! working space's. The result is a small RGBA8 image read back once per
//! submit, since the UI runs on its own wgpu device.

use std::sync::Arc;
use wgpu::util::DeviceExt;

/// The most columns the picture is measured into. It is measured into
/// one column per pixel of the widest trace drawn, so a scope drawn at its
/// pane's size keeps a one-pixel feature one pixel wide.
pub const MAX_BIN_COLS: u32 = 4096;
/// The fewest: for a small scope, or a measurement no waveform draws from.
const MIN_BIN_COLS: u32 = 256;
/// Levels the picture is measured into: 10-bit.
pub const BIN_ROWS: u32 = 1024;
/// Cells across the vectorscope's chroma plane, Cb and Cr each −0.5..0.5.
/// Twice the output size, so a 2× zoom still has a cell per pixel.
pub const VECTOR_BINS: u32 = 2048;

/// How the shade pass lays the channels out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveformMode {
    /// One trace of Y′, computed from the encoded channels with the
    /// measured space's luminance weights.
    Luma = 0,
    /// Red, green and blue traces on top of each other, additive.
    RgbOverlay = 1,
    /// Red, green and blue side by side.
    Parade = 2,
}

/// The transfer a linear sample is encoded with before it is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeTransfer {
    Srgb = 0,
    Bt1886 = 1,
    /// SMPTE ST 2084, with `scale` mapping linear 1.0 to a fraction of
    /// 10 000 cd/m²: display light where 1.0 is 203 nits passes 0.0203.
    Pq = 2,
}

/// The picture to measure.
pub enum ScopeInput<'a> {
    /// Linear RGBA f32. `matrix` takes the working space's primaries to
    /// the measured space's (identity to measure in the working space's
    /// own); `luma` weights the encoded channels for Y′; `transfer` and
    /// `scale` encode a sample before it is binned.
    Linear {
        rgba: &'a [f32],
        width: u32,
        height: u32,
        matrix: [[f32; 3]; 3],
        luma: [f32; 3],
        transfer: ScopeTransfer,
        scale: f32,
    },
    /// Display-encoded RGBA8: the bytes on screen, binned as the codes
    /// they are. `luma` weights the codes for Y′.
    Display {
        rgba: &'a [u8],
        width: u32,
        height: u32,
        luma: [f32; 3],
    },
    /// [`ScopeInput::Display`]'s bytes where the display encode left
    /// them, on this renderer's device: packed RGBA8, `width × height`
    /// of them, row after row. Measured in place, so nothing is read
    /// back or uploaded — at 4K the round trip was 33 MB and 24 ms for
    /// every pass while playing (#333).
    DisplayOnGpu {
        buffer: &'a wgpu::Buffer,
        width: u32,
        height: u32,
        luma: [f32; 3],
    },
}

impl ScopeInput<'_> {
    fn size(&self) -> (u32, u32) {
        match self {
            ScopeInput::Linear { width, height, .. }
            | ScopeInput::Display { width, height, .. }
            | ScopeInput::DisplayOnGpu { width, height, .. } => (*width, *height),
        }
    }
}

/// The gap between the parade's panels, in output pixels.
pub fn parade_gap(out_w: u32) -> u32 {
    (out_w / 128).clamp(2, 12)
}

/// One vectorscope picture: the density of chroma, Cb across and Cr up,
/// in the measured space's own Y′CbCr.
#[derive(Clone, Copy, Debug)]
pub struct VectorscopeJob {
    /// 1 shows the full ±0.5 plane; 2 shows ±0.25 at the same size.
    pub zoom: f32,
    /// Trace brightness, 0..1; negative takes the default.
    pub intensity: f32,
    /// Tint the trace by the hue at each point, or draw it mono.
    pub colorize: bool,
    /// Only samples whose Y′ lies in `luma_lo..=luma_hi` are plotted.
    pub luma_lo: f32,
    pub luma_hi: f32,
    pub glow: bool,
    pub out_w: u32,
    pub out_h: u32,
}

/// How the histogram lays its channels out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistogramLayout {
    /// Red, green and blue over each other in one panel, additive.
    Overlay,
    /// Red, green and blue side by side.
    Parade,
    /// Red, green and blue in rows, red on top.
    Stack,
    /// One distribution of Y′.
    Luma,
}

/// One histogram picture: across the whole frame, how many samples sit
/// at each level, level left to right and count upward.
#[derive(Clone, Copy, Debug)]
pub struct HistogramJob {
    pub layout: HistogramLayout,
    /// Height by the logarithm of the count, so a small population
    /// beside a large one still shows.
    pub log: bool,
    /// Brightness of the area under each curve, 0..1. The curve itself is
    /// always drawn in full.
    pub fill: f32,
    pub glow: bool,
    pub out_w: u32,
    pub out_h: u32,
}

/// One panel of a histogram, in output pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistogramPanel {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// The channels drawn in it: 1 red, 2 green, 4 blue, 8 Y′.
    pub channels: u32,
}

/// Where a histogram of `layout` draws at `out_w × out_h`. The shader
/// draws from exactly these rectangles, so a graticule laid on them lines
/// up with the curves.
pub fn histogram_panels(layout: HistogramLayout, out_w: u32, out_h: u32) -> Vec<HistogramPanel> {
    let gap = parade_gap(out_w);
    match layout {
        HistogramLayout::Overlay => vec![HistogramPanel { x: 0, y: 0, w: out_w, h: out_h, channels: 7 }],
        HistogramLayout::Luma => vec![HistogramPanel { x: 0, y: 0, w: out_w, h: out_h, channels: 8 }],
        HistogramLayout::Parade => {
            let w = out_w.saturating_sub(2 * gap) / 3;
            (0..3).map(|i| HistogramPanel { x: i * (w + gap), y: 0, w, h: out_h, channels: 1 << i }).collect()
        }
        HistogramLayout::Stack => {
            let h = out_h.saturating_sub(2 * gap) / 3;
            (0..3).map(|i| HistogramPanel { x: 0, y: i * (h + gap), w: out_w, h, channels: 1 << i }).collect()
        }
    }
}

/// One picture to draw from a frame's measurement.
#[derive(Clone, Copy, Debug)]
pub enum ScopeJob {
    Waveform(WaveformJob),
    Vectorscope(VectorscopeJob),
    Histogram(HistogramJob),
}

impl ScopeJob {
    /// The drawn image's width and height.
    pub fn out_size(&self) -> (u32, u32) {
        match self {
            ScopeJob::Waveform(w) => (w.out_w, w.out_h),
            ScopeJob::Vectorscope(v) => (v.out_w, v.out_h),
            ScopeJob::Histogram(h) => (h.out_w, h.out_h),
        }
    }
}

impl ScopeJob {
    fn out(&self) -> (u32, u32) {
        match self {
            ScopeJob::Waveform(j) => (j.out_w, j.out_h),
            ScopeJob::Vectorscope(j) => (j.out_w, j.out_h),
            ScopeJob::Histogram(j) => (j.out_w, j.out_h),
        }
    }
    fn glow(&self) -> bool {
        match self {
            ScopeJob::Waveform(j) => j.glow,
            ScopeJob::Vectorscope(j) => j.glow,
            ScopeJob::Histogram(j) => j.glow,
        }
    }
    /// How much of the glow is added back. A histogram's filled area is
    /// large, so its glow is kept to the light off the curve.
    fn glow_strength(&self) -> f32 {
        match self {
            ScopeJob::Histogram(_) => 0.35,
            _ => 0.6,
        }
    }
    fn gain(&self) -> f32 {
        let intensity = match self {
            ScopeJob::Waveform(j) => j.intensity,
            ScopeJob::Vectorscope(j) => j.intensity,
            ScopeJob::Histogram(_) => -1.0,
        };
        // The slider is geometric: 0 is a sixth of the default, 1 six
        // times it.
        let slider = if intensity < 0.0 { 1.0 } else { 6.0f32.powf(intensity.clamp(0.0, 1.0) * 2.0 - 1.0) };
        // The waveform's saturating curve reaches three quarters of full
        // brightness at a density of about 5.5, so only a dense trace is
        // bright and a busy picture stays readable; the vectorscope's log
        // curve takes its gain per unit of density.
        let base = if matches!(self, ScopeJob::Waveform(_)) { 0.25 } else { 1.5 };
        base * slider
    }
}

/// One picture to draw from the measured bins.
#[derive(Clone, Copy, Debug)]
pub struct WaveformJob {
    pub mode: WaveformMode,
    /// Trace brightness, 0..1; negative takes the default.
    pub intensity: f32,
    /// Bit mask of channels to draw in the RGB modes: 1 red, 2 green, 4
    /// blue.
    pub channels: u32,
    /// Draw each column's lowest and highest level as a line.
    pub envelope: bool,
    /// Glow the trace.
    pub glow: bool,
    pub out_w: u32,
    pub out_h: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AccParams {
    width: u32,
    height: u32,
    step: u32,
    cols: u32,
    rows: u32,
    transfer: u32,
    scale: f32,
    _pad: u32,
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
    luma: [f32; 4],
    lo: f32,
    hi: f32,
    _p2: u32,
    _p3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VecShadeParams {
    out_w: u32,
    out_h: u32,
    n: u32,
    colorize: u32,
    zoom: f32,
    gain: f32,
    norm: f32,
    floor: f32,
    luma: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ExtParams {
    cols: u32,
    rows: u32,
    _p0: u32,
    _p1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadeParams {
    out_w: u32,
    out_h: u32,
    cols: u32,
    rows: u32,
    mode: u32,
    channels: u32,
    envelope: u32,
    gap: u32,
    gain: f32,
    /// Levels per expected sample: `rows / (samples per column)`, so a
    /// picture spread evenly over the levels reads density 1.
    norm: f32,
    _f0: f32,
    _f1: f32,
}

/// The low-pass filter's width, in pixels of a picture 3840 pixels on its
/// long side; a smaller picture's filter is narrower in proportion.
const LOW_PASS_SIGMA: f32 = 4.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LpParams {
    width: u32,
    height: u32,
    out_w: u32,
    out_h: u32,
    /// The copy keeps every `r`-th pixel each way.
    r: u32,
    radius: u32,
    transfer: u32,
    _p0: u32,
    sigma: f32,
    scale: f32,
    _p1: f32,
    _p2: f32,
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    out_w: u32,
    out_h: u32,
    mode: u32,
    gap: u32,
    radius: u32,
    _p0: u32,
    sigma: f32,
    _p1: f32,
}

/// The fewest of a frame's pixels a scope measures, whatever the quality
/// asks: about a quarter of a 4K frame.
pub const MIN_SAMPLES: u64 = 2_000_000;
/// The samples an occupied pixel of a waveform should draw on, blurred
/// together, before its brightness is more measurement than chance.
const TRACE_GATHER: f64 = 8.0;
/// The share of a waveform's pixels its samples occupy, for turning the
/// sample count into samples per occupied pixel. A picture's levels
/// cluster; a quarter of the height per column is typical.
const TRACE_OCCUPANCY: f64 = 0.25;

/// The step a frame is sampled at: the quality's, but never so coarse that
/// fewer than [`MIN_SAMPLES`] of its pixels are measured. A small frame —
/// a skim's, a reduced playback frame — is measured whole: skipping pixels
/// of it saves almost nothing and leaves a large scope without samples.
pub fn sample_step(width: u32, height: u32, quality_step: u32) -> u32 {
    let want = (width as u64 * height as u64).min(MIN_SAMPLES);
    let mut step = quality_step.max(1);
    while step > 1 && (width.div_ceil(step) as u64) * (height.div_ceil(step) as u64) < want {
        step -= 1;
    }
    step
}

/// How far to blur a waveform's densities, across and up, in output
/// pixels; 0 for not at all.
///
/// A pixel's brightness is its density, and a density from a handful of
/// samples is chance: a trace drawn larger than its samples cover breaks
/// into specks. So the densities are blurred just enough that an occupied
/// pixel draws on about [`TRACE_GATHER`] samples, and not at all when
/// there are plenty, as a frame measured whole in a docked pane has.
/// Across, the blur is at least half the pixels one sampled column is
/// spread over, so a small frame's columns do not stand as bands. One
/// Gaussian for the whole trace: nothing steps, so nothing shows as a box.
fn trace_blur(sampled_w: f64, sampled_h: f64, trace_w: u32, out_h: u32) -> (f32, f32) {
    let area = (trace_w as f64 * out_h as f64 * TRACE_OCCUPANCY).max(1.0);
    let per_px = (sampled_w * sampled_h / area).max(1e-6);
    let sigma = (TRACE_GATHER / (std::f64::consts::TAU * per_px)).sqrt();
    let spread = trace_w as f64 / sampled_w.max(1.0);
    let used = |s: f64| if s < 0.6 { 0.0 } else { s.min(8.0) as f32 };
    (used(sigma.max(0.5 * spread)), used(sigma))
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HistShadeParams {
    out_w: u32,
    out_h: u32,
    rows: u32,
    panels: u32,
    log: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
    fill: f32,
    headroom: f32,
    _f0: f32,
    _f1: f32,
    rects: [[u32; 4]; 3],
    chans: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlowParams {
    out_w: u32,
    out_h: u32,
    radius: u32,
    glow: u32,
    sigma: f32,
    strength: f32,
    _p0: f32,
    _p1: f32,
}

/// A scope shader from `src/gpu/shaders/scopes/`, as the build script
/// assembled it (#3094): its `//#include` lines replaced by the shared
/// pieces in `shaders/include/`.
macro_rules! scope_shader {
    ($path:literal) => {
        include_str!(concat!(env!("OUT_DIR"), "/shaders/scopes/", $path))
    };
}

// The pieces several scope shaders share, in `shaders/include/`:
// `scope_lp_params`: the low-pass filter, a separable Gaussian over the encoded levels: what a waveform with its low-pass filter on measures, so fine texture, noise and the steps between 8-bit codes draw lines rather than a haze.
// `scope_lp_h`, the pass across: every `r`-th column of each row, the Gaussian of its neighbours.
// `scope_lut_common`: the viewing-LUT pre-pass (#466): the picture the scopes measure, run through a 3D LUT first and written as display codes, which the display path then bins. The lookup is on the encoded values, as the viewer's LUT pass reads them.

const SCOPE_ACC_LINEAR: &str = scope_shader!("acc_linear.wgsl");

const SCOPE_ACC_DISPLAY: &str = scope_shader!("acc_display.wgsl");

/// A low-passed copy's levels, already encoded, into the waveform bins.
const SCOPE_ACC_ENCODED: &str = scope_shader!("acc_encoded.wgsl");

/// The low-pass across a linear picture: its taps encoded as the waveform
/// measures them.
const SCOPE_LP_H_LINEAR: &str = scope_shader!("lp_h_linear.wgsl");

/// The low-pass across a display picture: its codes are the taps.
const SCOPE_LP_H_DISPLAY: &str = scope_shader!("lp_h_display.wgsl");

/// The low-pass down the rows the first pass smoothed: every `r`-th of
/// them, the Gaussian of its neighbours.
const SCOPE_LP_V: &str = scope_shader!("lp_v.wgsl");

const SCOPE_ACC_VECTOR_LINEAR: &str = scope_shader!("acc_vector_linear.wgsl");

const SCOPE_ACC_VECTOR_DISPLAY: &str = scope_shader!("acc_vector_display.wgsl");

const SCOPE_SHADE_VECTOR: &str = scope_shader!("shade_vector.wgsl");

/// Per column and channel: the lowest and highest occupied level.
const SCOPE_EXTENTS: &str = scope_shader!("extents.wgsl");

/// Running sums of the bins, first over levels within each column: with
/// [`SCOPE_SAT_ROWS`] after it, a summed-area table, so the shade pass sums
/// any rectangle of bins with four reads however large the rectangle.
const SCOPE_SAT_COLS: &str = scope_shader!("sat_cols.wgsl");

/// Then across columns, in place: each invocation owns one level of one
/// channel. The sums may wrap past 2^32; a rectangle taken from them with
/// the same wrapping arithmetic is still exact.
const SCOPE_SAT_ROWS: &str = scope_shader!("sat_rows.wgsl");

/// A waveform's density at every output pixel: per channel, the samples
/// in the bins the pixel covers, over whole 8-bit codes of level. Every
/// code's bins sum in or out together, so an 8-bit picture's comb, one
/// filled level in four, reads flat. Red, green, blue and Y' in the four
/// components; a parade pixel carries only its panel's channel.
const SCOPE_TRACE_DENSITY: &str = scope_shader!("trace_density.wgsl");

/// The densities blurred along rows, within each parade panel: a Gaussian
/// of the radius and sigma given, renormalised where it meets an edge, so
/// the mean is kept. A radius of 0 copies.
const SCOPE_TRACE_BLUR_H: &str = scope_shader!("trace_blur_h.wgsl");

/// The densities blurred along columns, the same way.
const SCOPE_TRACE_BLUR_V: &str = scope_shader!("trace_blur_v.wgsl");

/// A waveform's picture from its blurred densities: brightness, the
/// channels' colours, the envelope.
const SCOPE_TRACE_COLOUR: &str = scope_shader!("trace_colour.wgsl");

/// The histogram's counts: every column of the waveform's bins summed,
/// per level and channel. The histogram is the waveform's measurement
/// with the columns folded together, so it costs no pass over the frame.
const SCOPE_HIST_REDUCE: &str = scope_shader!("hist_reduce.wgsl");

/// Smooth the counts, and find the height that fills the panel.
///
/// The smoothing is a tent four levels to either side, weighted `1 − d/4`.
/// An 8-bit picture's codes land four 10-bit levels apart, and that tent
/// sums a comb of period four to a constant, so an untouched 8-bit
/// picture reads as the smooth distribution it is; a grade that stretches
/// the codes further apart still shows its gaps.
///
/// The peak ignores the outermost `EDGE` levels: samples clipped at black
/// or white pile up there, and scaling to that pile would flatten the rest
/// of the picture. The pile is still drawn, as a spike to the top.
const SCOPE_HIST_PREP: &str = scope_shader!("hist_prep.wgsl");

const SCOPE_SHADE_HIST: &str = scope_shader!("shade_hist.wgsl");

const SCOPE_GLOW_H: &str = scope_shader!("glow_h.wgsl");

const SCOPE_GLOW_V: &str = scope_shader!("glow_v.wgsl");

const SCOPE_LUT_LINEAR: &str = scope_shader!("lut_linear.wgsl");

const SCOPE_LUT_DISPLAY: &str = scope_shader!("lut_display.wgsl");




#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LutParams {
    width: u32,
    height: u32,
    step: u32,
    out_w: u32,
    out_h: u32,
    size: u32,
    transfer: u32,
    scale: f32,
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
}


























/// The measured bins and the pipelines that fill and draw them.
pub struct ScopeRenderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    acc_layout: wgpu::BindGroupLayout,
    acc_linear: wgpu::ComputePipeline,
    acc_display: wgpu::ComputePipeline,
    acc_encoded: wgpu::ComputePipeline,
    lp_layout: wgpu::BindGroupLayout,
    lp_h_linear: wgpu::ComputePipeline,
    lp_h_display: wgpu::ComputePipeline,
    lp_v: wgpu::ComputePipeline,
    /// Whether the waveform and parade measure a low-passed copy.
    low_pass: bool,
    /// The low-passed rows, then the low-passed copy.
    lp_tmp: Option<wgpu::Buffer>,
    lp_tmp_size: u64,
    lp_out: Option<wgpu::Buffer>,
    lp_out_size: u64,
    /// The histogram's counts in one column, while the waveform bins hold
    /// a low-passed copy's.
    hist_bins: wgpu::Buffer,
    acc_vector_linear: wgpu::ComputePipeline,
    acc_vector_display: wgpu::ComputePipeline,
    vshade_layout: wgpu::BindGroupLayout,
    vshade_pipeline: wgpu::ComputePipeline,
    vbins: wgpu::Buffer,
    hist_reduce_layout: wgpu::BindGroupLayout,
    hist_reduce_pipeline: wgpu::ComputePipeline,
    hist_prep_layout: wgpu::BindGroupLayout,
    hist_prep_pipeline: wgpu::ComputePipeline,
    hshade_layout: wgpu::BindGroupLayout,
    hshade_pipeline: wgpu::ComputePipeline,
    /// Per channel and level, the frame's count; then the smoothed count
    /// with each channel's peaks after it.
    hist_raw: wgpu::Buffer,
    hist: wgpu::Buffer,
    ext_layout: wgpu::BindGroupLayout,
    ext_pipeline: wgpu::ComputePipeline,
    shade_layout: wgpu::BindGroupLayout,
    shade_pipeline: wgpu::ComputePipeline,
    trace_colour_pipeline: wgpu::ComputePipeline,
    trace_blur_layout: wgpu::BindGroupLayout,
    trace_blur_h_pipeline: wgpu::ComputePipeline,
    trace_blur_v_pipeline: wgpu::ComputePipeline,
    glow_h_layout: wgpu::BindGroupLayout,
    glow_h_pipeline: wgpu::ComputePipeline,
    glow_v_layout: wgpu::BindGroupLayout,
    glow_v_pipeline: wgpu::ComputePipeline,
    input: Option<wgpu::Buffer>,
    input_size: u64,
    bins: wgpu::Buffer,
    extents: wgpu::Buffer,
    /// The columns `bins` and `extents` have room for.
    bin_cols: u32,
    /// The bins' summed-area table: what the waveform shade reads.
    sat: wgpu::Buffer,
    sat_cols_layout: wgpu::BindGroupLayout,
    sat_cols_pipeline: wgpu::ComputePipeline,
    sat_rows_layout: wgpu::BindGroupLayout,
    sat_rows_pipeline: wgpu::ComputePipeline,
    shaded: Option<wgpu::Buffer>,
    blurred: Option<wgpu::Buffer>,
    /// A waveform's per-pixel densities, blurred in place through `blurred`.
    dens: Option<wgpu::Buffer>,
    float_size: u64,
    packed: Vec<wgpu::Buffer>,
    staging: Option<wgpu::Buffer>,
    staging_size: u64,
    lut_layout: wgpu::BindGroupLayout,
    lut_linear: wgpu::ComputePipeline,
    lut_display: wgpu::ComputePipeline,
    /// The viewing LUT the scopes measure after (#466): its key, its
    /// size and its table on the device.
    lut: Option<(u64, u32, wgpu::Buffer)>,
    /// The picture through the LUT, as display codes.
    lut_out: Option<wgpu::Buffer>,
    lut_out_size: u64,
}

/// Every `step`-th pixel of `src` (RGBA, `width` wide), `w2 × h2`.
fn decimate_rgba<T: bytemuck::Pod + Send + Sync>(src: &[T], width: u32, w2: u32, h2: u32, step: u32) -> Vec<T> {
    use rayon::prelude::*;
    let row_len = (w2 as usize) * 4;
    let mut out = vec![T::zeroed(); row_len * (h2 as usize)];
    out.par_chunks_mut(row_len).enumerate().for_each(|(y, row)| {
        let src_row = (y as u32 * step * width) as usize * 4;
        for x in 0..w2 as usize {
            let i = src_row + x * (step as usize) * 4;
            row[x * 4..x * 4 + 4].copy_from_slice(&src[i..i + 4]);
        }
    });
    out
}

fn storage(read_only: bool) -> wgpu::BindingType {
    wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None }
}
const UNIFORM: wgpu::BindingType =
    wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None };

fn layout(device: &wgpu::Device, label: &str, types: &[wgpu::BindingType]) -> wgpu::BindGroupLayout {
    let entries: Vec<wgpu::BindGroupLayoutEntry> = types
        .iter()
        .enumerate()
        .map(|(i, ty)| wgpu::BindGroupLayoutEntry { binding: i as u32, visibility: wgpu::ShaderStages::COMPUTE, ty: *ty, count: None })
        .collect();
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries: &entries })
}

fn pipeline(device: &wgpu::Device, label: &str, bgl: &wgpu::BindGroupLayout, src: &str) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(src.into()) });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[bgl], push_constant_ranges: &[] });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pl),
        module: &module,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    })
}

fn buffer(device: &wgpu::Device, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
}

impl ScopeRenderer {
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let acc_layout = layout(&device, "scope_acc", &[storage(true), storage(false), UNIFORM]);
        let acc_linear = pipeline(&device, "scope_acc_linear", &acc_layout, SCOPE_ACC_LINEAR);
        let acc_display = pipeline(&device, "scope_acc_display", &acc_layout, SCOPE_ACC_DISPLAY);
        let acc_encoded = pipeline(&device, "scope_acc_encoded", &acc_layout, SCOPE_ACC_ENCODED);
        let lp_layout = layout(&device, "scope_lp", &[storage(true), storage(false), UNIFORM]);
        let lp_h_linear = pipeline(&device, "scope_lp_h_linear", &lp_layout, SCOPE_LP_H_LINEAR);
        let lp_h_display = pipeline(&device, "scope_lp_h_display", &lp_layout, SCOPE_LP_H_DISPLAY);
        let lp_v = pipeline(&device, "scope_lp_v", &lp_layout, SCOPE_LP_V);
        let hist_bins = buffer(&device, "scope_hist_bins", 4 * BIN_ROWS as u64 * 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let acc_vector_linear = pipeline(&device, "scope_acc_vector_linear", &acc_layout, SCOPE_ACC_VECTOR_LINEAR);
        let acc_vector_display = pipeline(&device, "scope_acc_vector_display", &acc_layout, SCOPE_ACC_VECTOR_DISPLAY);
        let vshade_layout = layout(&device, "scope_vshade", &[storage(true), storage(false), UNIFORM]);
        let vshade_pipeline = pipeline(&device, "scope_vshade", &vshade_layout, SCOPE_SHADE_VECTOR);
        let vbins = buffer(&device, "scope_vbins", (VECTOR_BINS * VECTOR_BINS) as u64 * 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let hist_reduce_layout = layout(&device, "scope_hist_reduce", &[storage(true), storage(false), UNIFORM]);
        let hist_reduce_pipeline = pipeline(&device, "scope_hist_reduce", &hist_reduce_layout, SCOPE_HIST_REDUCE);
        let hist_prep_layout = layout(&device, "scope_hist_prep", &[storage(true), storage(false), UNIFORM]);
        let hist_prep_pipeline = pipeline(&device, "scope_hist_prep", &hist_prep_layout, SCOPE_HIST_PREP);
        let hshade_layout = layout(&device, "scope_hshade", &[storage(true), storage(false), UNIFORM]);
        let hshade_pipeline = pipeline(&device, "scope_hshade", &hshade_layout, SCOPE_SHADE_HIST);
        let hist_raw = buffer(&device, "scope_hist_raw", 4 * BIN_ROWS as u64 * 4, wgpu::BufferUsages::STORAGE);
        let hist = buffer(&device, "scope_hist", (4 * BIN_ROWS as u64 + 8) * 4, wgpu::BufferUsages::STORAGE);
        let ext_layout = layout(&device, "scope_ext", &[storage(true), storage(false), UNIFORM]);
        let ext_pipeline = pipeline(&device, "scope_ext", &ext_layout, SCOPE_EXTENTS);
        let shade_layout = layout(&device, "scope_shade", &[storage(true), storage(true), storage(false), UNIFORM]);
        let shade_pipeline = pipeline(&device, "scope_trace_density", &shade_layout, SCOPE_TRACE_DENSITY);
        let trace_colour_pipeline = pipeline(&device, "scope_trace_colour", &shade_layout, SCOPE_TRACE_COLOUR);
        let trace_blur_layout = layout(&device, "scope_trace_blur", &[storage(true), storage(false), UNIFORM]);
        let trace_blur_h_pipeline = pipeline(&device, "scope_trace_blur_h", &trace_blur_layout, SCOPE_TRACE_BLUR_H);
        let trace_blur_v_pipeline = pipeline(&device, "scope_trace_blur_v", &trace_blur_layout, SCOPE_TRACE_BLUR_V);
        let glow_h_layout = layout(&device, "scope_glow_h", &[storage(true), storage(false), UNIFORM]);
        let glow_h_pipeline = pipeline(&device, "scope_glow_h", &glow_h_layout, SCOPE_GLOW_H);
        let glow_v_layout = layout(&device, "scope_glow_v", &[storage(true), storage(true), storage(false), UNIFORM]);
        let glow_v_pipeline = pipeline(&device, "scope_glow_v", &glow_v_layout, SCOPE_GLOW_V);
        let bins = buffer(&device, "scope_bins", 4 * (MIN_BIN_COLS * BIN_ROWS) as u64 * 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let extents = buffer(&device, "scope_extents", 4 * MIN_BIN_COLS as u64 * 2 * 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let sat = buffer(&device, "scope_sat", 4 * (MIN_BIN_COLS * BIN_ROWS) as u64 * 4, wgpu::BufferUsages::STORAGE);
        let sat_cols_layout = layout(&device, "scope_sat_cols", &[storage(true), storage(false), UNIFORM]);
        let sat_cols_pipeline = pipeline(&device, "scope_sat_cols", &sat_cols_layout, SCOPE_SAT_COLS);
        let sat_rows_layout = layout(&device, "scope_sat_rows", &[storage(false), UNIFORM]);
        let sat_rows_pipeline = pipeline(&device, "scope_sat_rows", &sat_rows_layout, SCOPE_SAT_ROWS);
        let lut_layout = layout(&device, "scope_lut", &[storage(true), storage(true), storage(false), UNIFORM]);
        let lut_linear = pipeline(&device, "scope_lut_linear", &lut_layout, SCOPE_LUT_LINEAR);
        let lut_display = pipeline(&device, "scope_lut_display", &lut_layout, SCOPE_LUT_DISPLAY);
        Self {
            device,
            queue,
            acc_layout,
            acc_linear,
            acc_display,
            acc_encoded,
            lp_layout,
            lp_h_linear,
            lp_h_display,
            lp_v,
            low_pass: false,
            lp_tmp: None,
            lp_tmp_size: 0,
            lp_out: None,
            lp_out_size: 0,
            hist_bins,
            acc_vector_linear,
            acc_vector_display,
            vshade_layout,
            vshade_pipeline,
            vbins,
            hist_reduce_layout,
            hist_reduce_pipeline,
            hist_prep_layout,
            hist_prep_pipeline,
            hshade_layout,
            hshade_pipeline,
            hist_raw,
            hist,
            ext_layout,
            ext_pipeline,
            shade_layout,
            shade_pipeline,
            trace_colour_pipeline,
            trace_blur_layout,
            trace_blur_h_pipeline,
            trace_blur_v_pipeline,
            glow_h_layout,
            glow_h_pipeline,
            glow_v_layout,
            glow_v_pipeline,
            input: None,
            input_size: 0,
            bins,
            extents,
            bin_cols: MIN_BIN_COLS,
            sat,
            sat_cols_layout,
            sat_cols_pipeline,
            sat_rows_layout,
            sat_rows_pipeline,
            shaded: None,
            blurred: None,
            dens: None,
            float_size: 0,
            packed: Vec::new(),
            staging: None,
            staging_size: 0,
            lut_layout,
            lut_linear,
            lut_display,
            lut: None,
            lut_out: None,
            lut_out_size: 0,
        }
    }

    /// Measure the picture after a viewing LUT (#466), or not. `lut` is a
    /// key that changes with the table, its size, and the table: r,g,b
    /// per grid point, red fastest, as `.cube` lays it out. The table is
    /// put on the device once per key.
    pub fn set_lut(&mut self, lut: Option<(u64, u32, &[f32])>) {
        match lut {
            None => self.lut = None,
            Some((key, size, table)) => {
                if self.lut.as_ref().is_some_and(|(k, _, _)| *k == key) {
                    return;
                }
                if size < 2 || table.len() < (size as usize).pow(3) * 3 {
                    self.lut = None;
                    return;
                }
                let buf = buffer(&self.device, "scope_lut_table", (table.len() * 4) as u64, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
                self.queue.write_buffer(&buf, 0, bytemuck::cast_slice(table));
                self.lut = Some((key, size, buf));
            }
        }
    }

    /// Measure the waveform and parade from a low-passed copy of the
    /// picture, so fine texture and noise draw lines rather than a haze.
    /// The histogram and vectorscope still measure the picture itself.
    pub fn set_low_pass(&mut self, on: bool) {
        self.low_pass = on;
    }

    /// The largest input the device binds in one storage buffer.
    fn max_input_bytes(&self) -> u64 {
        self.device.limits().max_storage_buffer_binding_size as u64
    }

    fn uniform<T: bytemuck::Pod>(&self, label: &str, v: &T) -> wgpu::Buffer {
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::bytes_of(v),
            usage: wgpu::BufferUsages::UNIFORM,
        })
    }

    /// Measure `input` every `step` pixels and draw every job from the
    /// measurement. One submit, one readback; the results are RGBA8 rows
    /// of each job's `out_w × out_h`, in job order, `None` where a job
    /// could not be drawn.
    pub fn render(&mut self, input: &ScopeInput<'_>, step: u32, jobs: &[ScopeJob]) -> Vec<Option<Vec<u8>>> {
        let Some((_, size, _)) = self.lut.as_ref() else {
            return self.render_measured(input, step, jobs);
        };
        let size = *size;
        let (width, height) = input.size();
        if width == 0 || height == 0 || jobs.is_empty() {
            return jobs.iter().map(|_| None).collect();
        }
        // Through the LUT first, at the step the scopes sample: the
        // codes that come out are what the display path then bins.
        let step = sample_step(width, height, step.max(1));
        let (out_w, out_h) = (width.div_ceil(step), height.div_ceil(step));
        let luma = match input {
            ScopeInput::Linear { luma, .. } | ScopeInput::Display { luma, .. } | ScopeInput::DisplayOnGpu { luma, .. } => *luma,
        };
        let (m, transfer, scale) = match input {
            ScopeInput::Linear { matrix, transfer, scale, .. } => (*matrix, *transfer as u32, *scale),
            _ => ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], 0, 1.0),
        };
        let uploaded: Option<wgpu::Buffer> = match input {
            ScopeInput::Linear { rgba, .. } => {
                if rgba.len() < (width as usize) * (height as usize) * 4 {
                    return jobs.iter().map(|_| None).collect();
                }
                let buf = buffer(&self.device, "scope_lut_in", (rgba.len() * 4) as u64, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
                self.queue.write_buffer(&buf, 0, bytemuck::cast_slice(rgba));
                Some(buf)
            }
            ScopeInput::Display { rgba, .. } => {
                if rgba.len() < (width as usize) * (height as usize) * 4 {
                    return jobs.iter().map(|_| None).collect();
                }
                let buf = buffer(&self.device, "scope_lut_in", rgba.len() as u64, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
                self.queue.write_buffer(&buf, 0, rgba);
                Some(buf)
            }
            ScopeInput::DisplayOnGpu { .. } => None,
        };
        let src: &wgpu::Buffer = match (&uploaded, input) {
            (Some(b), _) => b,
            (None, ScopeInput::DisplayOnGpu { buffer, .. }) => buffer,
            (None, _) => unreachable!("a CPU picture is uploaded"),
        };
        let need = out_w as u64 * out_h as u64 * 4;
        if self.lut_out.is_none() || self.lut_out_size < need {
            self.lut_out = Some(buffer(&self.device, "scope_lut_out", need, wgpu::BufferUsages::STORAGE));
            self.lut_out_size = need;
        }
        let params = self.uniform(
            "scope_lut_params",
            &LutParams {
                width,
                height,
                step,
                out_w,
                out_h,
                size,
                transfer,
                scale,
                m0: [m[0][0], m[0][1], m[0][2], 0.0],
                m1: [m[1][0], m[1][1], m[1][2], 0.0],
                m2: [m[2][0], m[2][1], m[2][2], 0.0],
            },
        );
        let (_, _, table) = self.lut.as_ref().expect("checked above");
        let out = self.lut_out.as_ref().expect("made above");
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_lut_bg"),
            layout: &self.lut_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: table.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scope_lut") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("scope_lut"), timestamp_writes: None });
            pass.set_pipeline(if matches!(input, ScopeInput::Linear { .. }) { &self.lut_linear } else { &self.lut_display });
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(out_w.div_ceil(16), out_h.div_ceil(16), 1);
        }
        self.queue.submit(Some(enc.finish()));
        let out = self.lut_out.take().expect("made above");
        let results = self.render_measured(&ScopeInput::DisplayOnGpu { buffer: &out, width: out_w, height: out_h, luma }, 1, jobs);
        self.lut_out = Some(out);
        results
    }

    /// [`Self::render`] of the picture as given, with no LUT.
    fn render_measured(&mut self, input: &ScopeInput<'_>, step: u32, jobs: &[ScopeJob]) -> Vec<Option<Vec<u8>>> {
        let step = step.max(1);
        let (width, height) = input.size();
        if width == 0 || height == 0 || jobs.is_empty() {
            return jobs.iter().map(|_| None).collect();
        }
        // The upload is the cost: a 4K linear frame is 132 MB. A frame
        // sampled every `step` pixels is decimated first so only the
        // samples travel, and a frame the device cannot bind whole (4K
        // DCI is past the default 128 MiB) is decimated by two besides.
        let step = sample_step(width, height, step);
        let decimate = match input {
            ScopeInput::Linear { .. } => {
                let need = (width as u64) * (height as u64) * 16;
                if need > self.max_input_bytes() { step.max(2) } else { step }
            }
            ScopeInput::Display { .. } => step,
            // Already on the device: the shaders sample every `step`th
            // pixel themselves.
            ScopeInput::DisplayOnGpu { .. } => 1,
        };
        let decimated_f: Vec<f32>;
        let decimated_u: Vec<u8>;
        let (bytes, width, height, step): (Option<&[u8]>, u32, u32, u32) = match input {
            ScopeInput::Linear { rgba, .. } => {
                if rgba.len() < (width as usize) * (height as usize) * 4 {
                    return jobs.iter().map(|_| None).collect();
                }
                if decimate > 1 {
                    let (w2, h2) = (width.div_ceil(decimate), height.div_ceil(decimate));
                    decimated_f = decimate_rgba::<f32>(rgba, width, w2, h2, decimate);
                    (Some(bytemuck::cast_slice(&decimated_f)), w2, h2, 1)
                } else {
                    (Some(bytemuck::cast_slice(rgba)), width, height, 1)
                }
            }
            ScopeInput::Display { rgba, .. } => {
                if rgba.len() < (width as usize) * (height as usize) * 4 {
                    return jobs.iter().map(|_| None).collect();
                }
                if decimate > 1 {
                    let (w2, h2) = (width.div_ceil(decimate), height.div_ceil(decimate));
                    decimated_u = decimate_rgba::<u8>(rgba, width, w2, h2, decimate);
                    (Some(&decimated_u), w2, h2, 1)
                } else {
                    (Some(rgba), width, height, 1)
                }
            }
            ScopeInput::DisplayOnGpu { buffer, .. } => {
                if buffer.size() < (width as u64) * (height as u64) * 4 {
                    return jobs.iter().map(|_| None).collect();
                }
                (None, width, height, step)
            }
        };
        // One column per pixel of the widest trace: a waveform's width, a
        // parade panel's. Any fewer and a thin vertical feature is spread
        // over several pixels of a large scope.
        let cols = jobs
            .iter()
            .filter_map(|j| match j {
                ScopeJob::Waveform(w) if w.mode == WaveformMode::Parade => Some(w.out_w.saturating_sub(2 * parade_gap(w.out_w)) / 3),
                ScopeJob::Waveform(w) => Some(w.out_w),
                _ => None,
            })
            .max()
            .unwrap_or(MIN_BIN_COLS)
            .clamp(MIN_BIN_COLS, MAX_BIN_COLS);
        // The low-pass filter: the waveform and parade measure a smoothed
        // copy of the picture. Smoothed by sigma 4, every second pixel
        // carries all the copy holds, so from there it keeps fewer.
        let sigma = LOW_PASS_SIGMA * width.max(height) as f32 / 3840.0;
        let lp = self.low_pass && sigma >= 0.5 && jobs.iter().any(|j| matches!(j, ScopeJob::Waveform(_)));
        let lp_r = if lp { ((sigma / 2.0).floor() as u32).max(1) } else { 1 };
        let (lp_w, lp_h) = (width.div_ceil(lp_r), height.div_ceil(lp_r));
        self.ensure_bins(cols);
        if let Some(bytes) = bytes {
            self.ensure_input(bytes.len() as u64);
        }
        if lp {
            self.ensure_low_pass(lp_w as u64 * height as u64 * 16, lp_w as u64 * lp_h as u64 * 16);
        }
        let input_buf: &wgpu::Buffer = match (bytes, input) {
            (Some(bytes), _) => {
                let buf = self.input.as_ref().expect("input buffer");
                self.queue.write_buffer(buf, 0, bytes);
                buf
            }
            (None, ScopeInput::DisplayOnGpu { buffer, .. }) => buffer,
            (None, _) => unreachable!("only a GPU input skips the upload"),
        };

        let (m, luma, transfer, scale) = match input {
            ScopeInput::Linear { matrix, luma, transfer, scale, .. } => (*matrix, *luma, *transfer as u32, *scale),
            ScopeInput::Display { luma, .. } | ScopeInput::DisplayOnGpu { luma, .. } => {
                ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], *luma, 0, 1.0)
            }
        };
        let want_hist = jobs.iter().any(|j| matches!(j, ScopeJob::Histogram(_)));
        let want_wave = want_hist || jobs.iter().any(|j| matches!(j, ScopeJob::Waveform(_)));
        let want_vector = jobs.iter().any(|j| matches!(j, ScopeJob::Vectorscope(_)));
        let (vlo, vhi) = jobs
            .iter()
            .find_map(|j| match j {
                ScopeJob::Vectorscope(v) => Some((v.luma_lo, v.luma_hi)),
                _ => None,
            })
            .unwrap_or((0.0, 1.0));
        let acc = AccParams {
            width,
            height,
            step,
            cols,
            rows: BIN_ROWS,
            transfer,
            scale,
            _pad: 0,
            m0: [m[0][0], m[0][1], m[0][2], 0.0],
            m1: [m[1][0], m[1][1], m[1][2], 0.0],
            m2: [m[2][0], m[2][1], m[2][2], 0.0],
            luma: [luma[0], luma[1], luma[2], 0.0],
            lo: 0.0,
            hi: 1.0,
            _p2: 0,
            _p3: 0,
        };
        let acc_params = self.uniform("scope_acc_params", &acc);
        let acc_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_acc_bg"),
            layout: &self.acc_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.bins.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: acc_params.as_entire_binding() },
            ],
        });
        // With the filter on, the waveform bins take the low-passed copy and
        // the histogram counts the picture itself, into one column.
        let lp_groups = if lp {
            let tmp = self.lp_tmp.as_ref().expect("low-pass rows");
            let copy = self.lp_out.as_ref().expect("low-pass copy");
            let lp_params = self.uniform(
                "scope_lp_params",
                &LpParams {
                    width,
                    height,
                    out_w: lp_w,
                    out_h: lp_h,
                    r: lp_r,
                    radius: (sigma * 3.0).ceil() as u32,
                    transfer,
                    _p0: 0,
                    sigma,
                    scale,
                    _p1: 0.0,
                    _p2: 0.0,
                    m0: acc.m0,
                    m1: acc.m1,
                    m2: acc.m2,
                },
            );
            let rows_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_lp_h_bg"),
                layout: &self.lp_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: input_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: tmp.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: lp_params.as_entire_binding() },
                ],
            });
            let copy_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_lp_v_bg"),
                layout: &self.lp_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: tmp.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: copy.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: lp_params.as_entire_binding() },
                ],
            });
            let wave_params = self.uniform("scope_lp_acc_params", &AccParams { width: lp_w, height: lp_h, ..acc });
            let wave_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_lp_acc_bg"),
                layout: &self.acc_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: copy.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.bins.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: wave_params.as_entire_binding() },
                ],
            });
            let count_params = self.uniform("scope_hist_acc_params", &AccParams { cols: 1, ..acc });
            let count_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_hist_acc_bg"),
                layout: &self.acc_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: input_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.hist_bins.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: count_params.as_entire_binding() },
                ],
            });
            let fold_params = self.uniform("scope_hist_one_params", &ExtParams { cols: 1, rows: BIN_ROWS, _p0: 0, _p1: 0 });
            let fold_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_hist_one_bg"),
                layout: &self.hist_reduce_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.hist_bins.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.hist_raw.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: fold_params.as_entire_binding() },
                ],
            });
            Some((rows_bg, copy_bg, wave_bg, count_bg, fold_bg, [lp_params, wave_params, count_params, fold_params]))
        } else {
            None
        };
        let vacc = AccParams { cols: VECTOR_BINS, rows: VECTOR_BINS, lo: vlo, hi: vhi, ..acc };
        let vacc_params = self.uniform("scope_vacc_params", &vacc);
        let vacc_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_vacc_bg"),
            layout: &self.acc_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.vbins.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: vacc_params.as_entire_binding() },
            ],
        });
        let ext_params = self.uniform("scope_ext_params", &ExtParams { cols, rows: BIN_ROWS, _p0: 0, _p1: 0 });
        let ext_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_ext_bg"),
            layout: &self.ext_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.bins.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.extents.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: ext_params.as_entire_binding() },
            ],
        });
        let sat_cols_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_sat_cols_bg"),
            layout: &self.sat_cols_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.bins.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.sat.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: ext_params.as_entire_binding() },
            ],
        });
        let sat_rows_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_sat_rows_bg"),
            layout: &self.sat_rows_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.sat.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: ext_params.as_entire_binding() },
            ],
        });
        let hist_params = self.uniform("scope_hist_params", &ExtParams { cols, rows: BIN_ROWS, _p0: 0, _p1: 0 });
        let hist_reduce_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_hist_reduce_bg"),
            layout: &self.hist_reduce_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.bins.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.hist_raw.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: hist_params.as_entire_binding() },
            ],
        });
        let hist_prep_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_hist_prep_bg"),
            layout: &self.hist_prep_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.hist_raw.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.hist.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: hist_params.as_entire_binding() },
            ],
        });

        // Samples per bin column, for the density normalisation; and for
        // the chroma plane, samples per cell were the picture spread over
        // the whole of it.
        let (wave_w, wave_h) = if lp { (lp_w, lp_h) } else { (width, height) };
        let sw = wave_w.div_ceil(step) as f64;
        let sh = wave_h.div_ceil(step) as f64;
        let per_column = (sw * sh / cols as f64).max(1.0);
        let norm = (BIN_ROWS as f64 / per_column) as f32;
        let total = (width.div_ceil(step) as f64 * height.div_ceil(step) as f64).max(1.0);
        let vnorm = ((VECTOR_BINS as f64 * VECTOR_BINS as f64) / total) as f32;

        let max_px = jobs.iter().map(|j| { let (w, h) = j.out(); (w as u64) * (h as u64) }).max().unwrap_or(0);
        if max_px == 0 {
            return jobs.iter().map(|_| None).collect();
        }
        self.ensure_float(max_px * 16);
        while self.packed.len() < jobs.len() {
            self.packed.push(buffer(&self.device, "scope_packed", 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC));
        }
        let mut staging_total = 0u64;
        for (i, job) in jobs.iter().enumerate() {
            let (out_w, out_h) = job.out();
            let bytes = (out_w as u64) * (out_h as u64) * 4;
            if self.packed[i].size() < bytes {
                self.packed[i] = buffer(&self.device, "scope_packed", bytes, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
            }
            staging_total += bytes.next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        }
        self.ensure_staging(staging_total);
        let staging = self.staging.as_ref().expect("staging");
        let shaded = self.shaded.as_ref().expect("shaded");
        let blurred = self.blurred.as_ref().expect("blurred");
        let dens = self.dens.as_ref().expect("dens");

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scopes") });
        if want_wave {
            encoder.clear_buffer(&self.bins, 0, None);
            encoder.clear_buffer(&self.extents, 0, None);
        }
        if lp && want_hist {
            encoder.clear_buffer(&self.hist_bins, 0, None);
        }
        if want_vector {
            encoder.clear_buffer(&self.vbins, 0, None);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("scope_measure"), timestamp_writes: None });
            let groups = (width.div_ceil(step).div_ceil(16), height.div_ceil(step).div_ceil(16));
            if want_wave {
                let direct = match input {
                    ScopeInput::Linear { .. } => &self.acc_linear,
                    ScopeInput::Display { .. } | ScopeInput::DisplayOnGpu { .. } => &self.acc_display,
                };
                if let Some((rows_bg, copy_bg, wave_bg, count_bg, _, _)) = &lp_groups {
                    pass.set_pipeline(match input {
                        ScopeInput::Linear { .. } => &self.lp_h_linear,
                        ScopeInput::Display { .. } | ScopeInput::DisplayOnGpu { .. } => &self.lp_h_display,
                    });
                    pass.set_bind_group(0, rows_bg, &[]);
                    pass.dispatch_workgroups(lp_w.div_ceil(16), height.div_ceil(16), 1);
                    pass.set_pipeline(&self.lp_v);
                    pass.set_bind_group(0, copy_bg, &[]);
                    pass.dispatch_workgroups(lp_w.div_ceil(16), lp_h.div_ceil(16), 1);
                    pass.set_pipeline(&self.acc_encoded);
                    pass.set_bind_group(0, wave_bg, &[]);
                    pass.dispatch_workgroups(lp_w.div_ceil(16), lp_h.div_ceil(16), 1);
                    if want_hist {
                        pass.set_pipeline(direct);
                        pass.set_bind_group(0, count_bg, &[]);
                        pass.dispatch_workgroups(groups.0, groups.1, 1);
                    }
                } else {
                    pass.set_pipeline(direct);
                    pass.set_bind_group(0, &acc_bg, &[]);
                    pass.dispatch_workgroups(groups.0, groups.1, 1);
                }
                if jobs.iter().any(|j| matches!(j, ScopeJob::Waveform(w) if w.envelope)) {
                    pass.set_pipeline(&self.ext_pipeline);
                    pass.set_bind_group(0, &ext_bg, &[]);
                    pass.dispatch_workgroups(cols.div_ceil(64), 4, 1);
                }
                if jobs.iter().any(|j| matches!(j, ScopeJob::Waveform(_))) {
                    pass.set_pipeline(&self.sat_cols_pipeline);
                    pass.set_bind_group(0, &sat_cols_bg, &[]);
                    // A workgroup of (64, 4) whose y is the channel: one
                    // row of workgroups, a column (at most 8K) or a bin row each.
                    pass.dispatch_workgroups(cols.div_ceil(64), 1, 1); // flat-ok: y is the channel
                    pass.set_pipeline(&self.sat_rows_pipeline);
                    pass.set_bind_group(0, &sat_rows_bg, &[]);
                    pass.dispatch_workgroups(BIN_ROWS.div_ceil(64), 1, 1); // flat-ok: y is the channel
                }
                if want_hist {
                    pass.set_pipeline(&self.hist_reduce_pipeline);
                    pass.set_bind_group(0, lp_groups.as_ref().map_or(&hist_reduce_bg, |g| &g.4), &[]);
                    crate::gpu::dispatch_flat(&mut pass, BIN_ROWS.div_ceil(64));
                    pass.set_pipeline(&self.hist_prep_pipeline);
                    pass.set_bind_group(0, &hist_prep_bg, &[]);
                    crate::gpu::dispatch_flat(&mut pass, 1);
                }
            }
            if want_vector {
                pass.set_pipeline(match input {
                    ScopeInput::Linear { .. } => &self.acc_vector_linear,
                    ScopeInput::Display { .. } | ScopeInput::DisplayOnGpu { .. } => &self.acc_vector_display,
                });
                pass.set_bind_group(0, &vacc_bg, &[]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
            }
        }
        let mut keep_alive: Vec<wgpu::Buffer> = Vec::new();
        let mut offsets = Vec::with_capacity(jobs.len());
        let mut offset = 0u64;
        for (i, job) in jobs.iter().enumerate() {
            let (out_w, out_h) = job.out();
            let (shade_pipeline, shade_bg, shade_params, trace) = match job {
                ScopeJob::Waveform(job) => {
                    let gap = if job.mode == WaveformMode::Parade { parade_gap(out_w) } else { 0 };
                    let trace_w = if job.mode == WaveformMode::Parade { out_w.saturating_sub(2 * gap) / 3 } else { out_w }.max(1);
                    let (sigma_h, sigma_v) = trace_blur(sw, sh, trace_w, out_h);
                    let shade = ShadeParams {
                        out_w,
                        out_h,
                        cols,
                        rows: BIN_ROWS,
                        mode: job.mode as u32,
                        channels: if job.channels & 7 == 0 { 7 } else { job.channels & 7 },
                        envelope: job.envelope as u32,
                        gap,
                        gain: ScopeJob::Waveform(*job).gain(),
                        norm,
                        _f0: 0.0,
                        _f1: 0.0,
                    };
                    let params = self.uniform("scope_shade_params", &shade);
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scope_trace_density_bg"),
                        layout: &self.shade_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: self.sat.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: self.extents.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: dens.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
                        ],
                    });
                    let colour_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scope_trace_colour_bg"),
                        layout: &self.shade_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: dens.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: self.extents.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: shaded.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
                        ],
                    });
                    let blur = |sigma: f32| BlurParams {
                        out_w,
                        out_h,
                        mode: job.mode as u32,
                        gap,
                        radius: if sigma > 0.0 { (sigma * 2.5).ceil().min(20.0) as u32 } else { 0 },
                        _p0: 0,
                        sigma: sigma.max(0.001),
                        _p1: 0.0,
                    };
                    let blur_h_params = self.uniform("scope_trace_blur_h_params", &blur(sigma_h));
                    let blur_v_params = self.uniform("scope_trace_blur_v_params", &blur(sigma_v));
                    let blur_h_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scope_trace_blur_h_bg"),
                        layout: &self.trace_blur_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: dens.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: blurred.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: blur_h_params.as_entire_binding() },
                        ],
                    });
                    let blur_v_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scope_trace_blur_v_bg"),
                        layout: &self.trace_blur_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: blurred.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: dens.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: blur_v_params.as_entire_binding() },
                        ],
                    });
                    (&self.shade_pipeline, bg, params, Some((blur_h_bg, blur_v_bg, colour_bg, [blur_h_params, blur_v_params])))
                }
                ScopeJob::Vectorscope(job) => {
                    let shade = VecShadeParams {
                        out_w,
                        out_h,
                        n: VECTOR_BINS,
                        colorize: job.colorize as u32,
                        zoom: job.zoom.clamp(0.25, 16.0),
                        gain: ScopeJob::Vectorscope(*job).gain(),
                        norm: vnorm,
                        floor: 0.05,
                        luma: [luma[0], luma[1], luma[2], 0.0],
                    };
                    let params = self.uniform("scope_vshade_params", &shade);
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scope_vshade_bg"),
                        layout: &self.vshade_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: self.vbins.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: shaded.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                        ],
                    });
                    (&self.vshade_pipeline, bg, params, None)
                }
                ScopeJob::Histogram(job) => {
                    let panels = histogram_panels(job.layout, out_w, out_h);
                    let mut rects = [[0u32; 4]; 3];
                    let mut chans = [0u32; 4];
                    for (i, panel) in panels.iter().enumerate() {
                        rects[i] = [panel.x, panel.y, panel.w, panel.h];
                        chans[i] = panel.channels;
                    }
                    let shade = HistShadeParams {
                        out_w,
                        out_h,
                        rows: BIN_ROWS,
                        panels: panels.len() as u32,
                        log: job.log as u32,
                        _p0: 0,
                        _p1: 0,
                        _p2: 0,
                        fill: job.fill.clamp(0.0, 1.0),
                        // The peak fills nine tenths of the panel, so the
                        // tallest curve still has its line below the top.
                        headroom: 0.9,
                        _f0: 0.0,
                        _f1: 0.0,
                        rects,
                        chans,
                    };
                    let params = self.uniform("scope_hshade_params", &shade);
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scope_hshade_bg"),
                        layout: &self.hshade_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: self.hist.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: shaded.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                        ],
                    });
                    (&self.hshade_pipeline, bg, params, None)
                }
            };
            let glow = GlowParams {
                out_w,
                out_h,
                radius: 8,
                glow: job.glow() as u32,
                sigma: 3.0,
                strength: job.glow_strength(),
                _p0: 0.0,
                _p1: 0.0,
            };
            let glow_params = self.uniform("scope_glow_params", &glow);
            let glow_h_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_glow_h_bg"),
                layout: &self.glow_h_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: shaded.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: blurred.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: glow_params.as_entire_binding() },
                ],
            });
            let glow_v_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_glow_v_bg"),
                layout: &self.glow_v_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: shaded.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: blurred.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.packed[i].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: glow_params.as_entire_binding() },
                ],
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("scope_draw"), timestamp_writes: None });
                let (gx, gy) = (out_w.div_ceil(16), out_h.div_ceil(16));
                pass.set_pipeline(shade_pipeline);
                pass.set_bind_group(0, &shade_bg, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
                if let Some((blur_h_bg, blur_v_bg, colour_bg, _)) = &trace {
                    pass.set_pipeline(&self.trace_blur_h_pipeline);
                    pass.set_bind_group(0, blur_h_bg, &[]);
                    pass.dispatch_workgroups(gx, gy, 1);
                    pass.set_pipeline(&self.trace_blur_v_pipeline);
                    pass.set_bind_group(0, blur_v_bg, &[]);
                    pass.dispatch_workgroups(gx, gy, 1);
                    pass.set_pipeline(&self.trace_colour_pipeline);
                    pass.set_bind_group(0, colour_bg, &[]);
                    pass.dispatch_workgroups(gx, gy, 1);
                }
                pass.set_pipeline(&self.glow_h_pipeline);
                pass.set_bind_group(0, &glow_h_bg, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
                pass.set_pipeline(&self.glow_v_pipeline);
                pass.set_bind_group(0, &glow_v_bg, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
            }
            let bytes = (out_w as u64) * (out_h as u64) * 4;
            encoder.copy_buffer_to_buffer(&self.packed[i], 0, staging, offset, bytes);
            offsets.push((offset, bytes));
            offset += bytes.next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
            keep_alive.push(shade_params);
            keep_alive.push(glow_params);
            if let Some((_, _, _, blur_params)) = trace {
                keep_alive.extend(blur_params);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = staging.slice(..staging_total);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let mut results: Vec<Option<Vec<u8>>> = jobs.iter().map(|_| None).collect();
        match crate::gpu::poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = slice.get_mapped_range();
                for (i, (off, len)) in offsets.iter().enumerate() {
                    results[i] = Some(data[*off as usize..(*off + *len) as usize].to_vec());
                }
                drop(data);
                staging.unmap();
            }
            other => {
                tracing::warn!("scope readback failed: {other:?}");
                staging.unmap();
            }
        }
        drop(keep_alive);
        results
    }

    fn ensure_bins(&mut self, cols: u32) {
        if self.bin_cols < cols {
            let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
            self.bins = buffer(&self.device, "scope_bins", 4 * cols as u64 * BIN_ROWS as u64 * 4, usage);
            self.extents = buffer(&self.device, "scope_extents", 4 * cols as u64 * 2 * 4, usage);
            self.sat = buffer(&self.device, "scope_sat", 4 * cols as u64 * BIN_ROWS as u64 * 4, wgpu::BufferUsages::STORAGE);
            self.bin_cols = cols;
        }
    }

    fn ensure_input(&mut self, bytes: u64) {
        if self.input.is_none() || self.input_size < bytes {
            self.input = Some(buffer(&self.device, "scope_input", bytes, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST));
            self.input_size = bytes;
        }
    }

    fn ensure_float(&mut self, bytes: u64) {
        if self.shaded.is_none() || self.float_size < bytes {
            self.shaded = Some(buffer(&self.device, "scope_shaded", bytes, wgpu::BufferUsages::STORAGE));
            self.blurred = Some(buffer(&self.device, "scope_blurred", bytes, wgpu::BufferUsages::STORAGE));
            self.dens = Some(buffer(&self.device, "scope_dens", bytes, wgpu::BufferUsages::STORAGE));
            self.float_size = bytes;
        }
    }

    fn ensure_low_pass(&mut self, tmp_bytes: u64, out_bytes: u64) {
        if self.lp_tmp.is_none() || self.lp_tmp_size < tmp_bytes {
            self.lp_tmp = Some(buffer(&self.device, "scope_lp_tmp", tmp_bytes, wgpu::BufferUsages::STORAGE));
            self.lp_tmp_size = tmp_bytes;
        }
        if self.lp_out.is_none() || self.lp_out_size < out_bytes {
            self.lp_out = Some(buffer(&self.device, "scope_lp_out", out_bytes, wgpu::BufferUsages::STORAGE));
            self.lp_out_size = out_bytes;
        }
    }

    fn ensure_staging(&mut self, bytes: u64) {
        if self.staging.is_none() || self.staging_size < bytes {
            self.staging = Some(buffer(&self.device, "scope_staging", bytes, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST));
            self.staging_size = bytes;
        }
    }
}

/// Every scope shader by name, as its pipeline compiles it: for the
/// shader tests (#3094).
#[cfg(test)]
pub(crate) fn shader_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        ("SCOPE_ACC_LINEAR", SCOPE_ACC_LINEAR),
        ("SCOPE_ACC_DISPLAY", SCOPE_ACC_DISPLAY),
        ("SCOPE_ACC_ENCODED", SCOPE_ACC_ENCODED),
        ("SCOPE_LP_H_LINEAR", SCOPE_LP_H_LINEAR),
        ("SCOPE_LP_H_DISPLAY", SCOPE_LP_H_DISPLAY),
        ("SCOPE_LP_V", SCOPE_LP_V),
        ("SCOPE_ACC_VECTOR_LINEAR", SCOPE_ACC_VECTOR_LINEAR),
        ("SCOPE_ACC_VECTOR_DISPLAY", SCOPE_ACC_VECTOR_DISPLAY),
        ("SCOPE_SHADE_VECTOR", SCOPE_SHADE_VECTOR),
        ("SCOPE_EXTENTS", SCOPE_EXTENTS),
        ("SCOPE_SAT_COLS", SCOPE_SAT_COLS),
        ("SCOPE_SAT_ROWS", SCOPE_SAT_ROWS),
        ("SCOPE_TRACE_DENSITY", SCOPE_TRACE_DENSITY),
        ("SCOPE_TRACE_BLUR_H", SCOPE_TRACE_BLUR_H),
        ("SCOPE_TRACE_BLUR_V", SCOPE_TRACE_BLUR_V),
        ("SCOPE_TRACE_COLOUR", SCOPE_TRACE_COLOUR),
        ("SCOPE_HIST_REDUCE", SCOPE_HIST_REDUCE),
        ("SCOPE_HIST_PREP", SCOPE_HIST_PREP),
        ("SCOPE_SHADE_HIST", SCOPE_SHADE_HIST),
        ("SCOPE_GLOW_H", SCOPE_GLOW_H),
        ("SCOPE_GLOW_V", SCOPE_GLOW_V),
        ("SCOPE_LUT_LINEAR", SCOPE_LUT_LINEAR),
        ("SCOPE_LUT_DISPLAY", SCOPE_LUT_DISPLAY),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scope shader parses and validates, as each pipeline is built
    /// from it. A shader wgpu rejects does not fail loudly here: the
    /// pipeline is invalid, its dispatches do nothing, and the scope is
    /// silently black.
    #[test]
    fn a_small_frame_is_measured_whole() {
        assert_eq!(sample_step(3840, 2160, 2), 2, "a 4K frame at medium quality: every second pixel, 2 M samples");
        assert_eq!(sample_step(1214, 2160, 2), 1, "a reduced playback frame: every pixel");
        assert_eq!(sample_step(608, 1080, 4), 1, "a skim frame: every pixel, whatever the quality");
        assert_eq!(sample_step(7680, 4320, 4), 4, "an 8K frame at low quality keeps its step");
        assert_eq!(sample_step(3840, 2160, 1), 1);
    }

    #[test]
    fn a_trace_is_blurred_only_when_its_samples_are_few() {
        // A 4K frame measured whole into a docked pane: plenty.
        assert_eq!(trace_blur(3840.0, 2160.0, 1750, 875), (0.0, 0.0));
        // A skim frame into a large pane: blurred, more across than the
        // samples alone ask, as each column stands for five pixels.
        let (h, v) = trace_blur(608.0, 1080.0, 2928, 880);
        assert!(v > 0.6 && h >= v && h >= 2.4, "skim frame blurred: {h} across, {v} up");
    }

    #[test]
    fn every_scope_shader_validates() {
        for (name, src) in shader_sources() {
            let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
        }
    }
}
