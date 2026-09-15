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
//! into a bin grid of [`BIN_COLS`] columns by [`BIN_ROWS`] levels. The
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

/// Columns the picture is measured into.
pub const BIN_COLS: u32 = 1024;
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
}

impl ScopeInput<'_> {
    fn size(&self) -> (u32, u32) {
        match self {
            ScopeInput::Linear { width, height, .. } | ScopeInput::Display { width, height, .. } => (*width, *height),
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
        // Density per unit of brightness. The slider is geometric: 0 is
        // a sixth of the default, 1 six times it.
        1.5 * if intensity < 0.0 { 1.0 } else { 6.0f32.powf(intensity.clamp(0.0, 1.0) * 2.0 - 1.0) }
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
    floor: f32,
    _pad: f32,
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

const WGSL_COMMON_ACC: &str = r#"
struct AccParams {
    width: u32, height: u32, step: u32, cols: u32,
    rows: u32, transfer: u32, scale: f32, _pad: u32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>, luma: vec4<f32>,
    lo: f32, hi: f32, _p2: u32, _p3: u32,
};
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> p: AccParams;

fn srgb_encode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.0031308) { return c * 12.92; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}
fn pq_encode(v: f32) -> f32 {
    let y = clamp(v, 0.0, 1.0);
    let n = pow(y, 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * n) / (1.0 + 18.6875 * n), 78.84375);
}
fn encode(v: f32) -> f32 {
    switch p.transfer {
        case 1u: { return pow(clamp(v, 0.0, 1.0), 1.0 / 2.4); }
        case 2u: { return pq_encode(v * p.scale); }
        default: { return srgb_encode(v); }
    }
}
// `wgt` units of channel `ch` at level `v` (0..1) into bin column `c`,
// split by distance between the two levels around it.
fn splat(ch: u32, c: u32, v: f32, wgt: f32) {
    let r = clamp(v, 0.0, 1.0) * f32(p.rows - 1u);
    let r0 = u32(floor(r));
    let fr = r - f32(r0);
    let r1 = min(r0 + 1u, p.rows - 1u);
    let base = ch * p.cols * p.rows + c * p.rows;
    atomicAdd(&bins[base + r0], u32((1.0 - fr) * wgt + 0.5));
    atomicAdd(&bins[base + r1], u32(fr * wgt + 0.5));
}
// One sample at source column `sx`. A sampled column stands for the
// `step` columns from it, and its 256 units go to the bin columns that
// span covers in proportion to the overlap. Exact coverage is what
// keeps a picture whose columns do not divide evenly into the bins from
// striping; it also means a sparse sampling leaves no bin column empty
// and a narrow picture fills the width.
fn bin_sample(sx: u32, e: vec3<f32>) {
    let y = dot(p.luma.xyz, e);
    let scale = f32(p.cols) / f32(p.width);
    let lo = f32(sx) * scale;
    let hi = min(f32(sx + p.step) * scale, f32(p.cols));
    let c_from = u32(floor(lo));
    let c_to = min(u32(ceil(hi)), p.cols);
    for (var c = c_from; c < c_to; c = c + 1u) {
        let overlap = min(f32(c + 1u), hi) - max(f32(c), lo);
        let wgt = 256.0 * overlap / (hi - lo);
        if (wgt < 0.5) { continue; }
        splat(0u, c, e.r, wgt);
        splat(1u, c, e.g, wgt);
        splat(2u, c, e.b, wgt);
        splat(3u, c, y, wgt);
    }
}
"#;

const WGSL_COMMON_VECTOR: &str = r#"
// One sample into the chroma plane: Cb across, Cr up, in the measured
// space's own Y′CbCr — `Cb = (B′ − Y′) / 2(1 − Kb)`, `Cr = (R′ − Y′) /
// 2(1 − Kr)` — split by distance between the four cells around it.
fn bin_vector(e: vec3<f32>) {
    let y = dot(p.luma.xyz, e);
    if (y < p.lo || y > p.hi) { return; }
    let cb = (e.b - y) * 0.5 / (1.0 - p.luma.z);
    let cr = (e.r - y) * 0.5 / (1.0 - p.luma.x);
    let n = f32(p.cols - 1u);
    let x = clamp((cb + 0.5) * n, 0.0, n);
    let yy = clamp((0.5 - cr) * n, 0.0, n);
    let x0 = u32(floor(x)); let y0 = u32(floor(yy));
    let fx = x - f32(x0); let fy = yy - f32(y0);
    let x1 = min(x0 + 1u, p.cols - 1u); let y1 = min(y0 + 1u, p.cols - 1u);
    atomicAdd(&bins[y0 * p.cols + x0], u32((1.0 - fx) * (1.0 - fy) * 256.0 + 0.5));
    atomicAdd(&bins[y0 * p.cols + x1], u32(fx * (1.0 - fy) * 256.0 + 0.5));
    atomicAdd(&bins[y1 * p.cols + x0], u32((1.0 - fx) * fy * 256.0 + 0.5));
    atomicAdd(&bins[y1 * p.cols + x1], u32(fx * fy * 256.0 + 0.5));
}
"#;

const WGSL_ACC_VECTOR_LINEAR: &str = r#"
@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = gid.x * p.step; let sy = gid.y * p.step;
    if (sx >= p.width || sy >= p.height) { return; }
    let c = src[sy * p.width + sx].rgb;
    let m = vec3<f32>(dot(p.m0.xyz, c), dot(p.m1.xyz, c), dot(p.m2.xyz, c));
    bin_vector(vec3<f32>(encode(m.r), encode(m.g), encode(m.b)));
}
"#;

const WGSL_ACC_VECTOR_DISPLAY: &str = r#"
@group(0) @binding(0) var<storage, read> src: array<u32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = gid.x * p.step; let sy = gid.y * p.step;
    if (sx >= p.width || sy >= p.height) { return; }
    let v = src[sy * p.width + sx];
    bin_vector(vec3<f32>(f32(v & 0xffu), f32((v >> 8u) & 0xffu), f32((v >> 16u) & 0xffu)) / 255.0);
}
"#;

const WGSL_SHADE_VECTOR: &str = r#"
struct VecShadeParams {
    out_w: u32, out_h: u32, n: u32, colorize: u32,
    zoom: f32, gain: f32, norm: f32, floor: f32,
    luma: vec4<f32>,
};
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: VecShadeParams;

// The colour of a chroma direction: the hue at (cb, cr) at full
// vividness, so the trace is tinted the way the picture is, where it is.
fn tint_of(cb: f32, cr: f32) -> vec3<f32> {
    let mag = max(length(vec2<f32>(cb, cr)), 1e-4);
    let ncb = cb / mag * 0.35; let ncr = cr / mag * 0.35;
    let kr = p.luma.x; let kg = p.luma.y; let kb = p.luma.z;
    let r = 0.5 + ncr * 2.0 * (1.0 - kr);
    let b = 0.5 + ncb * 2.0 * (1.0 - kb);
    let g = (0.5 - kr * r - kb * b) / kg;
    var c = clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
    c = c / max(max(c.r, c.g), max(c.b, 1e-3));
    return mix(c, vec3<f32>(1.0), 0.12);
}
fn glow_of(n: f32) -> f32 {
    return clamp(log2(1.0 + n * p.gain) / 8.0, 0.0, 1.0);
}
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let oy = gid.y;
    if (ox >= p.out_w || oy >= p.out_h) { return; }
    let oi = oy * p.out_w + ox;
    let n = f32(p.n);
    // The patch of the plane this pixel shows, in cells.
    let bx0 = ((f32(ox) / f32(p.out_w) - 0.5) / p.zoom + 0.5) * n;
    let bx1 = ((f32(ox + 1u) / f32(p.out_w) - 0.5) / p.zoom + 0.5) * n;
    let by0 = ((f32(oy) / f32(p.out_h) - 0.5) / p.zoom + 0.5) * n;
    let by1 = ((f32(oy + 1u) / f32(p.out_h) - 0.5) / p.zoom + 0.5) * n;
    if (bx1 <= 0.0 || bx0 >= n || by1 <= 0.0 || by0 >= n) {
        out[oi] = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        return;
    }
    // The patch, with a tent of eight cells around it on every side. An
    // 8-bit 4:2:0 picture's chroma is a lattice up to nine cells apart,
    // and the tent joins the lattice into the cloud the picture is;
    // eight cells is 0.4 % of range.
    let tent = 8.0;
    let area = (bx1 - bx0 + tent) * (by1 - by0 + tent);
    var sum = 0.0;
    let ix0 = u32(max(floor(bx0 - tent), 0.0)); let ix1 = min(u32(ceil(bx1 + tent)), p.n);
    let iy0 = u32(max(floor(by0 - tent), 0.0)); let iy1 = min(u32(ceil(by1 + tent)), p.n);
    for (var iy = iy0; iy < iy1; iy = iy + 1u) {
        let cy = f32(iy) + 0.5;
        let dy = max(max(by0 - cy, cy - by1), 0.0);
        let wy = max(1.0 - dy / tent, 0.0);
        for (var ix = ix0; ix < ix1; ix = ix + 1u) {
            let cx = f32(ix) + 0.5;
            let dx = max(max(bx0 - cx, cx - bx1), 0.0);
            let wx = max(1.0 - dx / tent, 0.0);
            sum = sum + f32(bins[iy * p.n + ix]) * wx * wy;
        }
    }
    if (sum <= 0.0) {
        out[oi] = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        return;
    }
    let dens = sum / 256.0 * p.norm / area;
    let i = max(glow_of(dens), p.floor);
    var t = vec3<f32>(0.80, 0.95, 0.85);
    if (p.colorize != 0u) {
        let cb = ((bx0 + bx1) * 0.5 / n) - 0.5;
        let cr = 0.5 - ((by0 + by1) * 0.5 / n);
        t = tint_of(cb, cr);
    }
    let rgb = mix(t, vec3<f32>(1.0), i * i * 0.5) * i;
    out[oi] = vec4<f32>(min(rgb, vec3<f32>(1.0)), 1.0);
}
"#;

const WGSL_ACC_LINEAR: &str = r#"
@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = gid.x * p.step; let sy = gid.y * p.step;
    if (sx >= p.width || sy >= p.height) { return; }
    let c = src[sy * p.width + sx].rgb;
    let m = vec3<f32>(dot(p.m0.xyz, c), dot(p.m1.xyz, c), dot(p.m2.xyz, c));
    bin_sample(sx, vec3<f32>(encode(m.r), encode(m.g), encode(m.b)));
}
"#;

const WGSL_ACC_DISPLAY: &str = r#"
@group(0) @binding(0) var<storage, read> src: array<u32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = gid.x * p.step; let sy = gid.y * p.step;
    if (sx >= p.width || sy >= p.height) { return; }
    let v = src[sy * p.width + sx];
    let e = vec3<f32>(f32(v & 0xffu), f32((v >> 8u) & 0xffu), f32((v >> 16u) & 0xffu)) / 255.0;
    bin_sample(sx, e);
}
"#;

/// Per column and channel: the lowest and highest occupied level.
const WGSL_EXTENTS: &str = r#"
struct ExtParams { cols: u32, rows: u32, _p0: u32, _p1: u32 };
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> ext: array<u32>;
@group(0) @binding(2) var<uniform> p: ExtParams;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = gid.x; let ch = gid.y;
    if (col >= p.cols || ch >= 4u) { return; }
    let base = ch * p.cols * p.rows + col * p.rows;
    var lo = 0xffffffffu; var hi = 0u; var any = false;
    for (var r = 0u; r < p.rows; r = r + 1u) {
        if (bins[base + r] != 0u) {
            if (!any) { lo = r; any = true; }
            hi = r;
        }
    }
    ext[(ch * p.cols + col) * 2u] = lo;
    ext[(ch * p.cols + col) * 2u + 1u] = select(0xffffffffu, hi, any);
}
"#;

const WGSL_SHADE: &str = r#"
struct ShadeParams {
    out_w: u32, out_h: u32, cols: u32, rows: u32,
    mode: u32, channels: u32, envelope: u32, gap: u32,
    gain: f32, norm: f32, floor: f32, _pad: f32,
};
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read> ext: array<u32>;
@group(0) @binding(2) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> p: ShadeParams;

// The trace colours. Red, green and blue are kept a little off pure so a
// lone blue trace reads on black and overlaps add towards white.
fn tint(ch: u32) -> vec3<f32> {
    switch ch {
        case 0u: { return vec3<f32>(1.0, 0.16, 0.12); }
        case 1u: { return vec3<f32>(0.18, 1.0, 0.22); }
        case 2u: { return vec3<f32>(0.26, 0.44, 1.0); }
        default: { return vec3<f32>(0.86, 0.93, 0.90); }
    }
}
// Density to brightness. Logarithmic, so a lone sample, a thin trace
// and a flat area are all visible at once and a dense core never turns
// into a flat blob: eight doublings of density span dark to full.
fn glow_of(n: f32) -> f32 {
    return clamp(log2(1.0 + n * p.gain) / 8.0, 0.0, 1.0);
}
// The count over levels [r0, r1) of column `c`, with a tent of four
// levels either side. Four levels is 0.4 % of range, below anything the
// graticule resolves, and a tent that wide is what keeps an 8-bit
// picture — whose levels land on every fourth 10-bit bin — from combing:
// a narrower one leaves the rows between two codes lighter or darker
// than the rows on them.
fn column_count(base: u32, r0: u32, r1: u32) -> f32 {
    var sum = 0.0;
    for (var r = r0; r < r1; r = r + 1u) {
        sum = sum + f32(bins[base + r]);
    }
    for (var d = 1u; d <= 4u; d = d + 1u) {
        let w = 1.0 - f32(d) * 0.2;
        if (r0 >= d) { sum = sum + f32(bins[base + r0 - d]) * w; }
        if (r1 + d - 1u < p.rows) { sum = sum + f32(bins[base + r1 + d - 1u]) * w; }
    }
    return sum;
}
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let oy = gid.y;
    if (ox >= p.out_w || oy >= p.out_h) { return; }
    let oi = oy * p.out_w + ox;

    // Which channels this pixel draws, and the panel it sits in.
    var ch_first = 0u; var ch_last = 2u;
    var panel_x0 = 0u; var panel_w = p.out_w;
    if (p.mode == 0u) {
        ch_first = 3u; ch_last = 3u;
    } else if (p.mode == 2u) {
        let pw = (p.out_w - 2u * p.gap) / 3u;
        var panel = 0u;
        if (ox >= pw + p.gap) { panel = 1u; }
        if (ox >= 2u * (pw + p.gap)) { panel = 2u; }
        panel_x0 = panel * (pw + p.gap);
        panel_w = pw;
        if (ox >= panel_x0 + pw) { out[oi] = vec4<f32>(0.0, 0.0, 0.0, 1.0); return; }
        ch_first = panel; ch_last = panel;
    }
    let px = ox - panel_x0;
    // The bins this pixel covers: columns by position, levels by value,
    // the top row being level 1.0.
    let c0 = (px * p.cols) / panel_w;
    let c1 = max(((px + 1u) * p.cols) / panel_w, c0 + 1u);
    let v_lo = 1.0 - f32(oy + 1u) / f32(p.out_h);
    let v_hi = 1.0 - f32(oy) / f32(p.out_h);
    let r0 = u32(floor(v_lo * f32(p.rows)));
    let r1 = min(max(u32(ceil(v_hi * f32(p.rows))), r0 + 1u), p.rows);
    // The tent adds two levels of weight either side.
    let cells = f32(c1 - c0) * (f32(r1 - r0) + 4.0);

    var rgb = vec3<f32>(0.0);
    for (var ch = ch_first; ch <= ch_last; ch = ch + 1u) {
        if (ch < 3u && (p.channels & (1u << ch)) == 0u) { continue; }
        var sum = 0.0; var edge = false;
        let base = ch * p.cols * p.rows;
        for (var c = c0; c < c1; c = c + 1u) {
            sum = sum + column_count(base + c * p.rows, r0, r1);
            if (p.envelope != 0u) {
                let lo = ext[(ch * p.cols + c) * 2u];
                let hi = ext[(ch * p.cols + c) * 2u + 1u];
                if ((lo >= r0 && lo < r1) || (hi >= r0 && hi < r1)) { edge = true; }
            }
        }
        if (sum <= 0.0 && !edge) { continue; }
        let n = sum / 256.0 * p.norm / cells;
        var i = glow_of(n);
        if (sum > 0.0) { i = max(i, p.floor); }
        if (edge) { i = max(i, 0.9); }
        // The core of a dense trace goes to white, as a phosphor does.
        let t = tint(ch);
        rgb = rgb + mix(t, vec3<f32>(1.0), i * i * 0.6) * i;
    }
    out[oi] = vec4<f32>(min(rgb, vec3<f32>(1.0)), 1.0);
}
"#;

/// The histogram's counts: every column of the waveform's bins summed,
/// per level and channel. The histogram is the waveform's measurement
/// with the columns folded together, so it costs no pass over the frame.
const WGSL_HIST_REDUCE: &str = r#"
struct HistParams { cols: u32, rows: u32, _p0: u32, _p1: u32 };
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> raw: array<f32>;
@group(0) @binding(2) var<uniform> p: HistParams;
@compute @workgroup_size(64, 4)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let level = gid.x; let ch = gid.y;
    if (level >= p.rows || ch >= 4u) { return; }
    let base = ch * p.cols * p.rows;
    var sum = 0.0;
    for (var c = 0u; c < p.cols; c = c + 1u) {
        sum = sum + f32(bins[base + c * p.rows + level]);
    }
    raw[ch * p.rows + level] = sum;
}
"#;

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
const WGSL_HIST_PREP: &str = r#"
struct HistParams { cols: u32, rows: u32, _p0: u32, _p1: u32 };
@group(0) @binding(0) var<storage, read> raw: array<f32>;
@group(0) @binding(1) var<storage, read_write> hist: array<f32>;
@group(0) @binding(2) var<uniform> p: HistParams;
const EDGE: u32 = 10u;
@compute @workgroup_size(1)
fn main() {
    let n = p.rows;
    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
        var inner = 0.0; var whole = 0.0;
        for (var l = 0u; l < n; l = l + 1u) {
            var s = raw[ch * n + l];
            for (var d = 1u; d < 4u; d = d + 1u) {
                let w = 1.0 - f32(d) * 0.25;
                if (l >= d) { s = s + raw[ch * n + l - d] * w; }
                if (l + d < n) { s = s + raw[ch * n + l + d] * w; }
            }
            hist[ch * n + l] = s;
            whole = max(whole, s);
            if (l >= EDGE && l + EDGE < n) { inner = max(inner, s); }
        }
        hist[4u * n + ch] = inner;
        hist[4u * n + 4u + ch] = whole;
    }
}
"#;

const WGSL_SHADE_HIST: &str = r#"
struct HistShadeParams {
    out_w: u32, out_h: u32, rows: u32, panels: u32,
    log: u32, _p0: u32, _p1: u32, _p2: u32,
    fill: f32, headroom: f32, _f0: f32, _f1: f32,
    rect0: vec4<u32>, rect1: vec4<u32>, rect2: vec4<u32>,
    chans: vec4<u32>,
};
@group(0) @binding(0) var<storage, read> hist: array<f32>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: HistShadeParams;

// The channel colours. Unlike the waveform's, each primary sums to the
// same 1.28, so where red, green and blue fill the same area they add to
// a neutral grey rather than a tint.
fn tint(ch: u32) -> vec3<f32> {
    switch ch {
        case 0u: { return vec3<f32>(1.0, 0.12, 0.10); }
        case 1u: { return vec3<f32>(0.12, 0.90, 0.18); }
        case 2u: { return vec3<f32>(0.16, 0.26, 1.0); }
        default: { return vec3<f32>(0.86, 0.93, 0.90); }
    }
}
fn rect(i: u32) -> vec4<u32> {
    switch i {
        case 0u: { return p.rect0; }
        case 1u: { return p.rect1; }
        default: { return p.rect2; }
    }
}
// The count every curve of this picture is scaled to: the tallest level
// of the channels drawn, away from the clipped ends.
fn peak() -> f32 {
    let n = p.rows;
    let mask = p.chans[0] | p.chans[1] | p.chans[2];
    var inner = 0.0; var whole = 0.0;
    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
        if ((mask & (1u << ch)) == 0u) { continue; }
        inner = max(inner, hist[4u * n + ch]);
        whole = max(whole, hist[4u * n + 4u + ch]);
    }
    if (inner > 0.0) { return inner; }
    return max(whole, 1.0);
}
// The curve's height over pixel column `px` of a panel `pw` wide, as a
// fraction of the panel: the tallest level the column covers, so a
// narrow spike keeps its height at any panel width.
fn height(ch: u32, px: i32, pw: u32, pk: f32) -> f32 {
    let x = u32(clamp(px, 0, i32(pw) - 1));
    let l0 = (x * p.rows) / pw;
    let l1 = min(max(((x + 1u) * p.rows) / pw, l0 + 1u), p.rows);
    var m = 0.0;
    for (var l = l0; l < l1; l = l + 1u) {
        m = max(m, hist[ch * p.rows + l]);
    }
    let v = m / pk;
    if (p.log != 0u) {
        // Three decades from the peak down to nothing.
        return min(log2(1.0 + v * 1024.0) / log2(1025.0) * p.headroom, 1.0);
    }
    return min(v * p.headroom, 1.0);
}
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let oy = gid.y;
    if (ox >= p.out_w || oy >= p.out_h) { return; }
    let oi = oy * p.out_w + ox;
    let pk = peak();
    var rgb = vec3<f32>(0.0);
    for (var i = 0u; i < p.panels; i = i + 1u) {
        let r = rect(i);
        if (ox < r.x || ox >= r.x + r.z || oy < r.y || oy >= r.y + r.w) { continue; }
        let px = i32(ox - r.x);
        let ph = f32(r.w);
        // This pixel's centre, in pixels above the panel's floor.
        let yc = ph - (f32(oy - r.y) + 0.5);
        for (var ch = 0u; ch < 4u; ch = ch + 1u) {
            if ((p.chans[i] & (1u << ch)) == 0u) { continue; }
            let h = height(ch, px, r.z, pk) * ph;
            let hl = height(ch, px - 1, r.z, pk) * ph;
            let hr = height(ch, px + 1, r.z, pk) * ph;
            // The area under the curve, anti-aliased at its top.
            let cover = clamp(h - (yc - 0.5), 0.0, 1.0);
            // The curve: the span it passes through in this column, joined
            // half way to each neighbour, so a steep flank is a continuous
            // line rather than a dot per column. Nothing along an empty floor.
            let lo = min(h, min((h + hl) * 0.5, (h + hr) * 0.5));
            let hi = max(h, max((h + hl) * 0.5, (h + hr) * 0.5));
            var line = 0.0;
            if (hi >= 0.5) {
                let d = max(max(lo - yc, yc - hi), 0.0);
                line = clamp(1.25 - d, 0.0, 1.0);
            }
            // The fill brightens towards the curve, as light collects at a
            // phosphor edge; at its fullest it stays dim enough that the
            // curves drawn over it read.
            let grad = 0.55 + 0.45 * clamp(yc / max(h, 1.0), 0.0, 1.0);
            let t = tint(ch);
            rgb = rgb + t * (cover * p.fill * 0.6 * grad) + mix(t, vec3<f32>(1.0), 0.35) * line;
        }
    }
    out[oi] = vec4<f32>(min(rgb, vec3<f32>(1.0)), 1.0);
}
"#;

const WGSL_GLOW_H: &str = r#"
struct GlowParams { out_w: u32, out_h: u32, radius: u32, glow: u32, sigma: f32, strength: f32, _p0: f32, _p1: f32 };
@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> tmp: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: GlowParams;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x; let y = gid.y;
    if (x >= p.out_w || y >= p.out_h) { return; }
    if (p.glow == 0u) { tmp[y * p.out_w + x] = vec4<f32>(0.0); return; }
    var acc = vec3<f32>(0.0); var wsum = 0.0;
    let r = i32(p.radius);
    for (var d = -r; d <= r; d = d + 1) {
        let sx = i32(x) + d;
        if (sx < 0 || sx >= i32(p.out_w)) { continue; }
        let w = exp(-f32(d * d) / (2.0 * p.sigma * p.sigma));
        acc = acc + src[y * p.out_w + u32(sx)].rgb * w;
        wsum = wsum + w;
    }
    tmp[y * p.out_w + x] = vec4<f32>(acc / wsum, 1.0);
}
"#;

const WGSL_GLOW_V: &str = r#"
struct GlowParams { out_w: u32, out_h: u32, radius: u32, glow: u32, sigma: f32, strength: f32, _p0: f32, _p1: f32 };
@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> tmp: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> packed: array<u32>;
@group(0) @binding(3) var<uniform> p: GlowParams;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x; let y = gid.y;
    if (x >= p.out_w || y >= p.out_h) { return; }
    var rgb = src[y * p.out_w + x].rgb;
    if (p.glow != 0u) {
        var acc = vec3<f32>(0.0); var wsum = 0.0;
        let r = i32(p.radius);
        for (var d = -r; d <= r; d = d + 1) {
            let sy = i32(y) + d;
            if (sy < 0 || sy >= i32(p.out_h)) { continue; }
            let w = exp(-f32(d * d) / (2.0 * p.sigma * p.sigma));
            acc = acc + tmp[u32(sy) * p.out_w + x].rgb * w;
            wsum = wsum + w;
        }
        rgb = rgb + acc / wsum * p.strength;
    }
    let c = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0 + 0.5;
    packed[y * p.out_w + x] = u32(c.r) | (u32(c.g) << 8u) | (u32(c.b) << 16u) | (255u << 24u);
}
"#;

/// The measured bins and the pipelines that fill and draw them.
pub struct ScopeRenderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    acc_layout: wgpu::BindGroupLayout,
    acc_linear: wgpu::ComputePipeline,
    acc_display: wgpu::ComputePipeline,
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
    glow_h_layout: wgpu::BindGroupLayout,
    glow_h_pipeline: wgpu::ComputePipeline,
    glow_v_layout: wgpu::BindGroupLayout,
    glow_v_pipeline: wgpu::ComputePipeline,
    input: Option<wgpu::Buffer>,
    input_size: u64,
    bins: wgpu::Buffer,
    extents: wgpu::Buffer,
    shaded: Option<wgpu::Buffer>,
    blurred: Option<wgpu::Buffer>,
    float_size: u64,
    packed: Vec<wgpu::Buffer>,
    staging: Option<wgpu::Buffer>,
    staging_size: u64,
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
        let acc_linear = pipeline(&device, "scope_acc_linear", &acc_layout, &format!("{WGSL_COMMON_ACC}{WGSL_ACC_LINEAR}"));
        let acc_display = pipeline(&device, "scope_acc_display", &acc_layout, &format!("{WGSL_COMMON_ACC}{WGSL_ACC_DISPLAY}"));
        let acc_vector_linear = pipeline(&device, "scope_acc_vector_linear", &acc_layout, &format!("{WGSL_COMMON_ACC}{WGSL_COMMON_VECTOR}{WGSL_ACC_VECTOR_LINEAR}"));
        let acc_vector_display = pipeline(&device, "scope_acc_vector_display", &acc_layout, &format!("{WGSL_COMMON_ACC}{WGSL_COMMON_VECTOR}{WGSL_ACC_VECTOR_DISPLAY}"));
        let vshade_layout = layout(&device, "scope_vshade", &[storage(true), storage(false), UNIFORM]);
        let vshade_pipeline = pipeline(&device, "scope_vshade", &vshade_layout, WGSL_SHADE_VECTOR);
        let vbins = buffer(&device, "scope_vbins", (VECTOR_BINS * VECTOR_BINS) as u64 * 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let hist_reduce_layout = layout(&device, "scope_hist_reduce", &[storage(true), storage(false), UNIFORM]);
        let hist_reduce_pipeline = pipeline(&device, "scope_hist_reduce", &hist_reduce_layout, WGSL_HIST_REDUCE);
        let hist_prep_layout = layout(&device, "scope_hist_prep", &[storage(true), storage(false), UNIFORM]);
        let hist_prep_pipeline = pipeline(&device, "scope_hist_prep", &hist_prep_layout, WGSL_HIST_PREP);
        let hshade_layout = layout(&device, "scope_hshade", &[storage(true), storage(false), UNIFORM]);
        let hshade_pipeline = pipeline(&device, "scope_hshade", &hshade_layout, WGSL_SHADE_HIST);
        let hist_raw = buffer(&device, "scope_hist_raw", 4 * BIN_ROWS as u64 * 4, wgpu::BufferUsages::STORAGE);
        let hist = buffer(&device, "scope_hist", (4 * BIN_ROWS as u64 + 8) * 4, wgpu::BufferUsages::STORAGE);
        let ext_layout = layout(&device, "scope_ext", &[storage(true), storage(false), UNIFORM]);
        let ext_pipeline = pipeline(&device, "scope_ext", &ext_layout, WGSL_EXTENTS);
        let shade_layout = layout(&device, "scope_shade", &[storage(true), storage(true), storage(false), UNIFORM]);
        let shade_pipeline = pipeline(&device, "scope_shade", &shade_layout, WGSL_SHADE);
        let glow_h_layout = layout(&device, "scope_glow_h", &[storage(true), storage(false), UNIFORM]);
        let glow_h_pipeline = pipeline(&device, "scope_glow_h", &glow_h_layout, WGSL_GLOW_H);
        let glow_v_layout = layout(&device, "scope_glow_v", &[storage(true), storage(true), storage(false), UNIFORM]);
        let glow_v_pipeline = pipeline(&device, "scope_glow_v", &glow_v_layout, WGSL_GLOW_V);
        let bins = buffer(&device, "scope_bins", 4 * (BIN_COLS * BIN_ROWS) as u64 * 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let extents = buffer(&device, "scope_extents", 4 * BIN_COLS as u64 * 2 * 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        Self {
            device,
            queue,
            acc_layout,
            acc_linear,
            acc_display,
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
            glow_h_layout,
            glow_h_pipeline,
            glow_v_layout,
            glow_v_pipeline,
            input: None,
            input_size: 0,
            bins,
            extents,
            shaded: None,
            blurred: None,
            float_size: 0,
            packed: Vec::new(),
            staging: None,
            staging_size: 0,
        }
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
        let step = step.max(1);
        let (width, height) = input.size();
        if width == 0 || height == 0 || jobs.is_empty() {
            return jobs.iter().map(|_| None).collect();
        }
        // The upload is the cost: a 4K linear frame is 132 MB. A frame
        // sampled every `step` pixels is decimated first so only the
        // samples travel, and a frame the device cannot bind whole (4K
        // DCI is past the default 128 MiB) is decimated by two besides.
        let decimate = match input {
            ScopeInput::Linear { .. } => {
                let need = (width as u64) * (height as u64) * 16;
                if need > self.max_input_bytes() { step.max(2) } else { step }
            }
            ScopeInput::Display { .. } => step,
        };
        let decimated_f: Vec<f32>;
        let decimated_u: Vec<u8>;
        let (bytes, width, height, step): (&[u8], u32, u32, u32) = match input {
            ScopeInput::Linear { rgba, .. } => {
                if rgba.len() < (width as usize) * (height as usize) * 4 {
                    return jobs.iter().map(|_| None).collect();
                }
                if decimate > 1 {
                    let (w2, h2) = (width.div_ceil(decimate), height.div_ceil(decimate));
                    decimated_f = decimate_rgba::<f32>(rgba, width, w2, h2, decimate);
                    (bytemuck::cast_slice(&decimated_f), w2, h2, 1)
                } else {
                    (bytemuck::cast_slice(rgba), width, height, 1)
                }
            }
            ScopeInput::Display { rgba, .. } => {
                if rgba.len() < (width as usize) * (height as usize) * 4 {
                    return jobs.iter().map(|_| None).collect();
                }
                if decimate > 1 {
                    let (w2, h2) = (width.div_ceil(decimate), height.div_ceil(decimate));
                    decimated_u = decimate_rgba::<u8>(rgba, width, w2, h2, decimate);
                    (&decimated_u, w2, h2, 1)
                } else {
                    (rgba, width, height, 1)
                }
            }
        };
        self.ensure_input(bytes.len() as u64);
        let input_buf = self.input.as_ref().expect("input buffer");
        self.queue.write_buffer(input_buf, 0, bytes);

        let (m, luma, transfer, scale) = match input {
            ScopeInput::Linear { matrix, luma, transfer, scale, .. } => (*matrix, *luma, *transfer as u32, *scale),
            ScopeInput::Display { luma, .. } => ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], *luma, 0, 1.0),
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
            cols: BIN_COLS,
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
        let ext_params = self.uniform("scope_ext_params", &ExtParams { cols: BIN_COLS, rows: BIN_ROWS, _p0: 0, _p1: 0 });
        let ext_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scope_ext_bg"),
            layout: &self.ext_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.bins.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.extents.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: ext_params.as_entire_binding() },
            ],
        });
        let hist_params = self.uniform("scope_hist_params", &ExtParams { cols: BIN_COLS, rows: BIN_ROWS, _p0: 0, _p1: 0 });
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
        let sw = width.div_ceil(step) as f64;
        let sh = height.div_ceil(step) as f64;
        let total = (sw * sh).max(1.0);
        let per_column = (total / BIN_COLS as f64).max(1.0);
        let norm = (BIN_ROWS as f64 / per_column) as f32;
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

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scopes") });
        if want_wave {
            encoder.clear_buffer(&self.bins, 0, None);
            encoder.clear_buffer(&self.extents, 0, None);
        }
        if want_vector {
            encoder.clear_buffer(&self.vbins, 0, None);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("scope_measure"), timestamp_writes: None });
            let groups = (width.div_ceil(step).div_ceil(16), height.div_ceil(step).div_ceil(16));
            if want_wave {
                pass.set_pipeline(match input {
                    ScopeInput::Linear { .. } => &self.acc_linear,
                    ScopeInput::Display { .. } => &self.acc_display,
                });
                pass.set_bind_group(0, &acc_bg, &[]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
                if jobs.iter().any(|j| matches!(j, ScopeJob::Waveform(w) if w.envelope)) {
                    pass.set_pipeline(&self.ext_pipeline);
                    pass.set_bind_group(0, &ext_bg, &[]);
                    pass.dispatch_workgroups(BIN_COLS.div_ceil(64), 4, 1);
                }
                if want_hist {
                    pass.set_pipeline(&self.hist_reduce_pipeline);
                    pass.set_bind_group(0, &hist_reduce_bg, &[]);
                    pass.dispatch_workgroups(BIN_ROWS.div_ceil(64), 1, 1);
                    pass.set_pipeline(&self.hist_prep_pipeline);
                    pass.set_bind_group(0, &hist_prep_bg, &[]);
                    pass.dispatch_workgroups(1, 1, 1);
                }
            }
            if want_vector {
                pass.set_pipeline(match input {
                    ScopeInput::Linear { .. } => &self.acc_vector_linear,
                    ScopeInput::Display { .. } => &self.acc_vector_display,
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
            let (shade_pipeline, shade_bg, shade_params) = match job {
                ScopeJob::Waveform(job) => {
                    let shade = ShadeParams {
                        out_w,
                        out_h,
                        cols: BIN_COLS,
                        rows: BIN_ROWS,
                        mode: job.mode as u32,
                        channels: if job.channels & 7 == 0 { 7 } else { job.channels & 7 },
                        envelope: job.envelope as u32,
                        gap: if job.mode == WaveformMode::Parade { parade_gap(out_w) } else { 0 },
                        gain: ScopeJob::Waveform(*job).gain(),
                        norm,
                        floor: 0.04,
                        _pad: 0.0,
                    };
                    let params = self.uniform("scope_shade_params", &shade);
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scope_shade_bg"),
                        layout: &self.shade_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: self.bins.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: self.extents.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: shaded.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
                        ],
                    });
                    (&self.shade_pipeline, bg, params)
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
                    (&self.vshade_pipeline, bg, params)
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
                    (&self.hshade_pipeline, bg, params)
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
            self.float_size = bytes;
        }
    }

    fn ensure_staging(&mut self, bytes: u64) {
        if self.staging.is_none() || self.staging_size < bytes {
            self.staging = Some(buffer(&self.device, "scope_staging", bytes, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST));
            self.staging_size = bytes;
        }
    }
}
