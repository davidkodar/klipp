//! Waveform and parade scopes drawn on the GPU.
//!
//! A scope is a density plot: for every column of the picture, how many
//! samples sit at each level. Drawing one well is two separate jobs, and
//! this module keeps them apart.
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
    /// of `out_w × out_h`, in job order, `None` where a job could not be
    /// drawn.
    pub fn render(&mut self, input: &ScopeInput<'_>, step: u32, jobs: &[WaveformJob]) -> Vec<Option<Vec<u8>>> {
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

        // Samples per bin column, for the density normalisation.
        let sw = width.div_ceil(step) as f64;
        let sh = height.div_ceil(step) as f64;
        let per_column = (sw * sh / BIN_COLS as f64).max(1.0);
        let norm = (BIN_ROWS as f64 / per_column) as f32;

        let max_px = jobs.iter().map(|j| (j.out_w as u64) * (j.out_h as u64)).max().unwrap_or(0);
        if max_px == 0 {
            return jobs.iter().map(|_| None).collect();
        }
        self.ensure_float(max_px * 16);
        while self.packed.len() < jobs.len() {
            self.packed.push(buffer(&self.device, "scope_packed", 4, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC));
        }
        let mut staging_total = 0u64;
        for (i, job) in jobs.iter().enumerate() {
            let bytes = (job.out_w as u64) * (job.out_h as u64) * 4;
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
        encoder.clear_buffer(&self.bins, 0, None);
        encoder.clear_buffer(&self.extents, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("scope_measure"), timestamp_writes: None });
            pass.set_pipeline(match input {
                ScopeInput::Linear { .. } => &self.acc_linear,
                ScopeInput::Display { .. } => &self.acc_display,
            });
            pass.set_bind_group(0, &acc_bg, &[]);
            pass.dispatch_workgroups(width.div_ceil(step).div_ceil(16), height.div_ceil(step).div_ceil(16), 1);
            if jobs.iter().any(|j| j.envelope) {
                pass.set_pipeline(&self.ext_pipeline);
                pass.set_bind_group(0, &ext_bg, &[]);
                pass.dispatch_workgroups(BIN_COLS.div_ceil(64), 4, 1);
            }
        }
        let mut keep_alive: Vec<wgpu::Buffer> = Vec::new();
        let mut offsets = Vec::with_capacity(jobs.len());
        let mut offset = 0u64;
        for (i, job) in jobs.iter().enumerate() {
            // Density per unit of brightness. The slider is geometric: 0
            // is a sixth of the default, 1 six times it.
            let gain = 1.5 * if job.intensity < 0.0 { 1.0 } else { 6.0f32.powf(job.intensity.clamp(0.0, 1.0) * 2.0 - 1.0) };
            let shade = ShadeParams {
                out_w: job.out_w,
                out_h: job.out_h,
                cols: BIN_COLS,
                rows: BIN_ROWS,
                mode: job.mode as u32,
                channels: if job.channels & 7 == 0 { 7 } else { job.channels & 7 },
                envelope: job.envelope as u32,
                gap: if job.mode == WaveformMode::Parade { parade_gap(job.out_w) } else { 0 },
                gain,
                norm,
                floor: 0.04,
                _pad: 0.0,
            };
            let shade_params = self.uniform("scope_shade_params", &shade);
            let shade_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scope_shade_bg"),
                layout: &self.shade_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.bins.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.extents.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: shaded.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: shade_params.as_entire_binding() },
                ],
            });
            let glow = GlowParams {
                out_w: job.out_w,
                out_h: job.out_h,
                radius: 8,
                glow: job.glow as u32,
                sigma: 3.0,
                strength: 0.6,
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
                let (gx, gy) = (job.out_w.div_ceil(16), job.out_h.div_ceil(16));
                pass.set_pipeline(&self.shade_pipeline);
                pass.set_bind_group(0, &shade_bg, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
                pass.set_pipeline(&self.glow_h_pipeline);
                pass.set_bind_group(0, &glow_h_bg, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
                pass.set_pipeline(&self.glow_v_pipeline);
                pass.set_bind_group(0, &glow_v_bg, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
            }
            let bytes = (job.out_w as u64) * (job.out_h as u64) * 4;
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
