//! Optical-flow frame interpolation on the GPU (#2215): the passes of
//! `crate::flow`, each shader a line-for-line mirror of its CPU
//! reference function, so the two agree to within float rounding.
//!
//! A pair of source frames is uploaded and its flow estimated once, then
//! kept ([`FlowKey`]): a clip slowed to a quarter draws four in-between
//! frames from one pair, and only the first pays for the upload and the
//! estimate. A kept pair is used again only for the same frames: its key
//! names them and a fingerprint of their pixels checks them, so a frame
//! that changed under the same key (a relink, a proxy, another grade
//! before a smooth cut) is estimated afresh. Kept pairs are bounded by
//! count and by bytes, and a pair far from the one asked for (a seek,
//! another clip) is dropped first, so two flows close together, each
//! dropping the other's pairs, estimate every frame afresh.
use super::{poll_until_mapped, GpuEffectsPipeline};
use crate::flow::{self, Interpolated};
use wgpu::util::DeviceExt;

/// The most source pairs kept at once.
const KEPT_PAIRS: usize = 3;

/// The most bytes kept pairs may hold together; the newest is kept
/// whatever its size. A 4K pair holds about 270 MB.
const KEPT_BYTES: u64 = 640 * 1024 * 1024;

/// A pair kept further than this from the one asked for, in source
/// seconds, is dropped: playback has moved on, or seeked away.
const KEPT_SPAN_SECS: f64 = 2.0;

/// Which pair of source frames a flow is between: the clip they are
/// read for (its source, as converted for it) and their presentation
/// times.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowKey {
    pub clip: u128,
    pub from_secs: f64,
    pub to_secs: f64,
}

/// A source pair uploaded and its flow estimated.
struct Pair {
    key: FlowKey,
    size: (u32, u32),
    /// [`fingerprint`] of the two frames uploaded.
    print: u64,
    a: wgpu::Buffer,
    b: wgpu::Buffer,
    forward: wgpu::Buffer,
    backward: wgpu::Buffer,
    count: wgpu::Buffer,
    work: (u32, u32),
    /// Read back with the pair's first frame; kept after.
    unreliable: Option<f32>,
    bytes: u64,
}

pub(super) struct FlowGpu {
    luma: wgpu::ComputePipeline,
    halve: wgpu::ComputePipeline,
    search: wgpu::ComputePipeline,
    smooth: wgpu::ComputePipeline,
    count: wgpu::ComputePipeline,
    synth: wgpu::ComputePipeline,
    pairs: Vec<Pair>,
    /// The drawn frame's buffer and its read-back copy (with room for the
    /// count after it), kept for the next frame of the same size.
    frame: Option<(u64, wgpu::Buffer, wgpu::Buffer)>,
}

/// FNV-1a over every 997th value of `a` and `b` and their lengths: what
/// tells two pairs of frames apart under one key, without reading every
/// pixel (a 4K pair is about 66 million values).
fn fingerprint(a: &[f32], b: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    let mut eat = |v: u64| {
        for byte in v.to_le_bytes() {
            h = (h ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    for frame in [a, b] {
        eat(frame.len() as u64);
        for v in frame.iter().step_by(997) {
            eat(u64::from(v.to_bits()));
        }
    }
    h
}

/// The uniform every pass reads at binding 0: the level it runs on and,
/// for a level's search, the one above it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    w: u32,
    h: u32,
    cw: u32,
    ch: u32,
    off: u32,
    coff: u32,
    src_w: u32,
    src_h: u32,
    lr: f32,
    lg: f32,
    lb: f32,
    t: f32,
    total: u32,
    frac: f32,
    _pad: [u32; 2],
}

impl Params {
    fn level(w: u32, h: u32, off: u32) -> Self {
        Params { w, h, cw: 0, ch: 0, off, coff: 0, src_w: 0, src_h: 0, lr: 0.0, lg: 0.0, lb: 0.0, t: 0.0, total: 0, frac: flow::UNRELIABLE_FRACTION, _pad: [0; 2] }
    }
}

/// The uniform and the constants every pass shares.
fn prelude() -> String {
    format!(
        "struct P {{ w: u32, h: u32, cw: u32, ch: u32, off: u32, coff: u32, src_w: u32, src_h: u32, lr: f32, lg: f32, lb: f32, t: f32, total: u32, frac: f32, _p0: u32, _p1: u32 }}
@group(0) @binding(0) var<uniform> p: P;
const SEARCH_RADIUS: i32 = {sr};
const PATCH_RADIUS: i32 = {pr};
const MOTION_PENALTY: f32 = {mp:?};
const CONSISTENCY_SCALE: f32 = {cs:?};
const PHOTOMETRIC_SCALE: f32 = {ps:?};
const UNRELIABLE_DISAGREEMENT: f32 = {ud:?};
const UNRELIABLE_COST: f32 = {uc:?};
fn encode(c: vec4<f32>) -> f32 {{ return sqrt(max(c.x * p.lr + c.y * p.lg + c.z * p.lb, 0.0)); }}
",
        sr = flow::SEARCH_RADIUS,
        pr = flow::PATCH_RADIUS,
        mp = flow::MOTION_PENALTY,
        cs = flow::CONSISTENCY_SCALE,
        ps = flow::PHOTOMETRIC_SCALE,
        ud = flow::UNRELIABLE_DISAGREEMENT,
        uc = flow::UNRELIABLE_COST,
    )
}

/// `name_at` and `name_sample` over a plane of floats in `buf` at `off`,
/// `w` × `h`: `Plane::at` and `Plane::sample`.
fn plane_fns(name: &str, buf: &str, off: &str, w: &str, h: &str) -> String {
    format!(
        "fn {name}_at(x: i32, y: i32) -> f32 {{
    let xc = clamp(x, 0, i32({w}) - 1);
    let yc = clamp(y, 0, i32({h}) - 1);
    return {buf}[{off} + u32(yc) * {w} + u32(xc)];
}}
fn {name}_sample(xi: f32, yi: f32) -> f32 {{
    let x = clamp(xi, 0.0, f32({w} - 1u));
    let y = clamp(yi, 0.0, f32({h} - 1u));
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let a = i32(x0);
    let b = i32(y0);
    let top = {name}_at(a, b) + ({name}_at(a + 1, b) - {name}_at(a, b)) * fx;
    let bottom = {name}_at(a, b + 1) + ({name}_at(a + 1, b + 1) - {name}_at(a, b + 1)) * fx;
    return top + (bottom - top) * fy;
}}
"
    )
}

/// `name_at` and `name_motion` over a flow field in `buf` at `off`, `w` ×
/// `h`: `Field::at` and `Field::motion`.
fn field_fns(name: &str, buf: &str, off: &str, w: &str, h: &str) -> String {
    format!(
        "fn {name}_at(x: i32, y: i32) -> vec4<f32> {{
    let xc = clamp(x, 0, i32({w}) - 1);
    let yc = clamp(y, 0, i32({h}) - 1);
    return {buf}[{off} + u32(yc) * {w} + u32(xc)];
}}
fn {name}_motion(xi: f32, yi: f32) -> vec2<f32> {{
    let x = clamp(xi, 0.0, f32({w} - 1u));
    let y = clamp(yi, 0.0, f32({h} - 1u));
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let a = i32(x0);
    let b = i32(y0);
    let top = {name}_at(a, b).xy + ({name}_at(a + 1, b).xy - {name}_at(a, b).xy) * fx;
    let bottom = {name}_at(a, b + 1).xy + ({name}_at(a + 1, b + 1).xy - {name}_at(a, b + 1).xy) * fx;
    return top + (bottom - top) * fy;
}}
"
    )
}

/// `work_luma`: the frame's encoded luma at level 0.
fn shader_luma() -> String {
    format!(
        "{}
@group(0) @binding(1) var<storage, read> frame: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if (id.x >= p.w || id.y >= p.h) {{ return; }}
    let y0 = id.y * p.src_h / p.h;
    let y1 = max((id.y + 1u) * p.src_h / p.h, y0 + 1u);
    let x0 = id.x * p.src_w / p.w;
    let x1 = max((id.x + 1u) * p.src_w / p.w, x0 + 1u);
    var sum = 0.0;
    for (var sy = y0; sy < y1; sy++) {{
        for (var sx = x0; sx < x1; sx++) {{
            sum += encode(frame[sy * p.src_w + sx]);
        }}
    }}
    out[id.y * p.w + id.x] = sum / f32((x1 - x0) * (y1 - y0));
}}
",
        prelude()
    )
}

/// `halve`: the level at `off` from the one above it at `coff`.
fn shader_halve() -> String {
    format!(
        "{}
@group(0) @binding(1) var<storage, read_write> lum: array<f32>;
{}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if (id.x >= p.w || id.y >= p.h) {{ return; }}
    let x = i32(id.x) * 2;
    let y = i32(id.y) * 2;
    lum[p.off + id.y * p.w + id.x] = (up_at(x, y) + up_at(x + 1, y) + up_at(x, y + 1) + up_at(x + 1, y + 1)) * 0.25;
}}
",
        prelude(),
        plane_fns("up", "lum", "p.coff", "p.cw", "p.ch")
    )
}

/// `search`: one level's raw field, written from index 0 of `raw`.
fn shader_search() -> String {
    format!(
        "{}
@group(0) @binding(1) var<storage, read> from_l: array<f32>;
@group(0) @binding(2) var<storage, read> to_l: array<f32>;
@group(0) @binding(3) var<storage, read> coarse: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> raw: array<vec4<f32>>;
{}{}{}
fn cost(x: i32, y: i32, pred: vec2<f32>, dx: i32, dy: i32) -> f32 {{
    let mx = pred.x + f32(dx);
    let my = pred.y + f32(dy);
    var sum = 0.0;
    for (var oy = -PATCH_RADIUS; oy <= PATCH_RADIUS; oy++) {{
        for (var ox = -PATCH_RADIUS; ox <= PATCH_RADIUS; ox++) {{
            sum += abs(from_at(x + ox, y + oy) - to_sample(f32(x + ox) + mx, f32(y + oy) + my));
        }}
    }}
    let n = (2 * PATCH_RADIUS + 1) * (2 * PATCH_RADIUS + 1);
    return sum / f32(n) + MOTION_PENALTY * f32(abs(dx) + abs(dy));
}}
fn parabola(minus: f32, centre: f32, plus: f32) -> f32 {{
    let denom = minus - 2.0 * centre + plus;
    if (denom > 1e-6) {{ return clamp((minus - plus) / (2.0 * denom), -0.5, 0.5); }}
    return 0.0;
}}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if (id.x >= p.w || id.y >= p.h) {{ return; }}
    let x = i32(id.x);
    let y = i32(id.y);
    var pred = vec2<f32>(0.0, 0.0);
    if (p.cw > 0u) {{
        let m = coarse_motion((f32(x) + 0.5) * 0.5 - 0.5, (f32(y) + 0.5) * 0.5 - 0.5);
        pred = vec2<f32>(m.x * 2.0, m.y * 2.0);
    }}
    var bx = 0;
    var by = 0;
    var bc = 1e30;
    for (var dy = -SEARCH_RADIUS; dy <= SEARCH_RADIUS; dy++) {{
        for (var dx = -SEARCH_RADIUS; dx <= SEARCH_RADIUS; dx++) {{
            let c = cost(x, y, pred, dx, dy);
            if (c < bc) {{ bx = dx; by = dy; bc = c; }}
        }}
    }}
    let sx = parabola(cost(x, y, pred, bx - 1, by), bc, cost(x, y, pred, bx + 1, by));
    let sy = parabola(cost(x, y, pred, bx, by - 1), bc, cost(x, y, pred, bx, by + 1));
    raw[id.y * p.w + id.x] = vec4<f32>(pred.x + f32(bx) + sx, pred.y + f32(by) + sy, bc, 0.0);
}}
",
        prelude(),
        plane_fns("from", "from_l", "p.off", "p.w", "p.h"),
        plane_fns("to", "to_l", "p.off", "p.w", "p.h"),
        field_fns("coarse", "coarse", "p.coff", "p.cw", "p.ch"),
    )
}

/// `smooth`: the level's field at `off` from the raw one.
fn shader_smooth() -> String {
    format!(
        "{}
@group(0) @binding(1) var<storage, read> raw: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> field: array<vec4<f32>>;
{}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if (id.x >= p.w || id.y >= p.h) {{ return; }}
    let x = i32(id.x);
    let y = i32(id.y);
    var sx = 0.0;
    var sy = 0.0;
    var sw = 0.0;
    for (var oy = -1; oy <= 1; oy++) {{
        for (var ox = -1; ox <= 1; ox++) {{
            let v = raw_at(x + ox, y + oy);
            let weight = 1.0 / (v.z + 0.01);
            sx += v.x * weight;
            sy += v.y * weight;
            sw += weight;
        }}
    }}
    field[p.off + id.y * p.w + id.x] = vec4<f32>(sx / sw, sy / sw, raw_at(x, y).z, 0.0);
}}
",
        prelude(),
        field_fns("raw", "raw", "0u", "p.w", "p.h")
    )
}

/// `unreliable_share`'s count, at level 0.
fn shader_count() -> String {
    format!(
        "{}
@group(0) @binding(1) var<storage, read> fwd: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> bwd: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> count: array<atomic<u32>>;
{}{}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if (id.x >= p.w || id.y >= p.h) {{ return; }}
    let x = f32(id.x);
    let y = f32(id.y);
    let v = fwd_at(i32(id.x), i32(id.y));
    let tx = x + v.x;
    let ty = y + v.y;
    let inside = tx >= 0.0 && ty >= 0.0 && tx <= f32(p.w - 1u) && ty <= f32(p.h - 1u);
    let f = fwd_motion(x, y);
    let b = bwd_motion(x + f.x, y + f.y);
    let dis = sqrt((f.x + b.x) * (f.x + b.x) + (f.y + b.y) * (f.y + b.y));
    if ((inside && dis > UNRELIABLE_DISAGREEMENT) || v.z > UNRELIABLE_COST) {{
        atomicAdd(&count[0], 1u);
    }}
}}
",
        prelude(),
        field_fns("fwd", "fwd", "0u", "p.w", "p.h"),
        field_fns("bwd", "bwd", "0u", "p.w", "p.h"),
    )
}

/// `synthesise`: the in-between frame at `p.t`, at the frames' size
/// (`src_w` × `src_h`) from the flow at the work size (`w` × `h`).
fn shader_synth() -> String {
    format!(
        "{}
@group(0) @binding(1) var<storage, read> fa: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> fb: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> fwd: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> bwd: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read> count: array<u32>;
@group(0) @binding(6) var<storage, read_write> out: array<vec4<f32>>;
{}{}
fn sample_rgba(from_b: bool, xi: f32, yi: f32) -> vec4<f32> {{
    let x = clamp(xi, 0.0, f32(p.src_w - 1u));
    let y = clamp(yi, 0.0, f32(p.src_h - 1u));
    let x0f = floor(x);
    let y0f = floor(y);
    let fx = x - x0f;
    let fy = y - y0f;
    let x0 = u32(x0f);
    let y0 = u32(y0f);
    let x1 = min(x0 + 1u, p.src_w - 1u);
    let y1 = min(y0 + 1u, p.src_h - 1u);
    var c00: vec4<f32>; var c10: vec4<f32>; var c01: vec4<f32>; var c11: vec4<f32>;
    if (from_b) {{
        c00 = fb[y0 * p.src_w + x0]; c10 = fb[y0 * p.src_w + x1]; c01 = fb[y1 * p.src_w + x0]; c11 = fb[y1 * p.src_w + x1];
    }} else {{
        c00 = fa[y0 * p.src_w + x0]; c10 = fa[y0 * p.src_w + x1]; c01 = fa[y1 * p.src_w + x0]; c11 = fa[y1 * p.src_w + x1];
    }}
    let top = c00 + (c10 - c00) * fx;
    let bottom = c01 + (c11 - c01) * fx;
    return top + (bottom - top) * fy;
}}
fn inside(px: f32, py: f32) -> bool {{
    return px >= -0.5 && py >= -0.5 && px <= f32(p.src_w) - 0.5 && py <= f32(p.src_h) - 0.5;
}}
// `disagreement` with `forward` the forward field when `from_a`.
fn disagreement(from_a: bool, x: f32, y: f32) -> f32 {{
    var f: vec2<f32>;
    var b: vec2<f32>;
    if (from_a) {{
        f = fwd_motion(x, y);
        b = bwd_motion(x + f.x, y + f.y);
    }} else {{
        f = bwd_motion(x, y);
        b = fwd_motion(x + f.x, y + f.y);
    }}
    return sqrt((f.x + b.x) * (f.x + b.x) + (f.y + b.y) * (f.y + b.y));
}}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if (id.x >= p.src_w || id.y >= p.src_h) {{ return; }}
    let i = id.y * p.src_w + id.x;
    let t = clamp(p.t, 0.0, 1.0);
    let a0 = fa[i];
    let b0 = fb[i];
    let blend = a0 + (b0 - a0) * t;
    if (f32(count[0]) / f32(p.total) > p.frac) {{
        out[i] = blend;
        return;
    }}
    let kx = f32(p.w) / f32(p.src_w);
    let ky = f32(p.h) / f32(p.src_h);
    let x = f32(id.x);
    let y = f32(id.y);
    let qx = (x + 0.5) * kx - 0.5;
    let qy = (y + 0.5) * ky - 0.5;
    let f01 = fwd_motion(qx, qy);
    let f10 = bwd_motion(qx, qy);
    let s = 1.0 - t;
    let ax = x + (-s * t * f01.x + t * t * f10.x) / kx;
    let ay = y + (-s * t * f01.y + t * t * f10.y) / ky;
    let bx = x + (s * s * f01.x - t * s * f10.x) / kx;
    let by = y + (s * s * f01.y - t * s * f10.y) / ky;
    var ra = 0.0;
    if (inside(ax, ay)) {{
        let e = disagreement(true, (ax + 0.5) * kx - 0.5, (ay + 0.5) * ky - 0.5) / CONSISTENCY_SCALE;
        ra = exp(-e * e);
    }}
    var rb = 0.0;
    if (inside(bx, by)) {{
        let e = disagreement(false, (bx + 0.5) * kx - 0.5, (by + 0.5) * ky - 0.5) / CONSISTENCY_SCALE;
        rb = exp(-e * e);
    }}
    let ca = sample_rgba(false, ax, ay);
    let cb = sample_rgba(true, bx, by);
    let wa = s * ra;
    let wb = t * rb;
    if (wa + wb <= 1e-3) {{
        out[i] = blend;
        return;
    }}
    let d = abs(encode(ca) - encode(cb)) / PHOTOMETRIC_SCALE;
    let alike = exp(-d * d);
    let confidence = max(ra, rb) * (1.0 + (alike - 1.0) * min(ra, rb));
    let warped = (wa * ca + wb * cb) / (wa + wb);
    out[i] = blend + (warped - blend) * confidence;
}}
",
        prelude(),
        field_fns("fwd", "fwd", "0u", "p.w", "p.h"),
        field_fns("bwd", "bwd", "0u", "p.w", "p.h"),
    )
}

fn pipeline(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &module,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    })
}

/// The flow's shaders, by pipeline: what [`FlowGpu::new`] compiles, and
/// what the WGSL validation reads (#2344).
pub(super) fn sources() -> [(&'static str, String); 6] {
    [
        ("flow_luma", shader_luma()),
        ("flow_halve", shader_halve()),
        ("flow_search", shader_search()),
        ("flow_smooth", shader_smooth()),
        ("flow_count", shader_count()),
        ("flow_synth", shader_synth()),
    ]
}

impl FlowGpu {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let [luma, halve, search, smooth, count, synth] = sources().map(|(label, source)| pipeline(device, label, &source));
        FlowGpu {
            luma,
            halve,
            search,
            smooth,
            count,
            synth,
            pairs: Vec::new(),
            frame: None,
        }
    }
}

/// One compute pass of `pipeline` over `w` × `h`, its bindings in order
/// from 0, the first `params`.
fn pass(device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, pipeline: &wgpu::ComputePipeline, params: &Params, buffers: &[&wgpu::Buffer], (w, h): (u32, u32)) {
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("flow_params"), contents: bytemuck::bytes_of(params), usage: wgpu::BufferUsages::UNIFORM });
    let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
    entries.extend(buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32 + 1, resource: b.as_entire_binding() }));
    let layout = pipeline.get_bind_group_layout(0);
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("flow"), layout: &layout, entries: &entries });
    let mut cpass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("flow"), timestamp_writes: None });
    cpass.set_pipeline(pipeline);
    cpass.set_bind_group(0, &group, &[]);
    cpass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
}

impl GpuEffectsPipeline {
    /// The in-between frame at `t` between source frames `a` and `b`
    /// (linear RGBA, `width` × `height`) by optical flow, as
    /// [`crate::flow::interpolate`] draws it (#2215). The pair's flow is
    /// estimated once and kept under `key`, so the pair's later frames
    /// only warp. `luma` is the working space's luma weights. `None`
    /// without a device, or when a frame isn't `width` × `height`.
    #[allow(clippy::too_many_arguments)]
    pub fn interpolate_by_flow(&mut self, key: FlowKey, a: &[f32], b: &[f32], width: u32, height: u32, t: f32, luma: [f32; 3]) -> Option<Interpolated> {
        let pixels = (width as usize) * (height as usize);
        if self.is_device_lost() || width == 0 || height == 0 || a.len() != pixels * 4 || b.len() != pixels * 4 {
            return None;
        }
        let device = self.device.clone();
        // Anything recorded ahead of this goes first.
        self.flush();
        let flow_gpu = self.flow_gpu.get_or_insert_with(|| FlowGpu::new(&device));
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flow") });
        let print = fingerprint(a, b);
        let index = match flow_gpu.pairs.iter().position(|p| p.key == key && p.size == (width, height) && p.print == print) {
            Some(i) => i,
            None => {
                // Pairs far from this one first, then the oldest, past the
                // bounds; room made for the new pair.
                flow_gpu.pairs.retain(|p| p.key.clip == key.clip && p.key != key && (p.key.from_secs - key.from_secs).abs() <= KEPT_SPAN_SECS);
                let mut pair = estimate_pair(&device, &self.queue, flow_gpu, &mut enc, key, a, b, (width, height), luma);
                pair.print = print;
                flow_gpu.pairs.push(pair);
                while flow_gpu.pairs.len() > KEPT_PAIRS || (flow_gpu.pairs.len() > 1 && flow_gpu.pairs.iter().map(|p| p.bytes).sum::<u64>() > KEPT_BYTES) {
                    flow_gpu.pairs.remove(0);
                }
                flow_gpu.pairs.len() - 1
            }
        };
        let frame_bytes = pixels as u64 * 16;
        if flow_gpu.frame.as_ref().is_none_or(|(bytes, _, _)| *bytes != frame_bytes) {
            let out = device.create_buffer(&wgpu::BufferDescriptor { label: Some("flow_out"), size: frame_bytes, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
            let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("flow_read"), size: frame_bytes + 16, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            flow_gpu.frame = Some((frame_bytes, out, staging));
        }
        // Out for the draw, back after it.
        let (kept_bytes, out, staging) = flow_gpu.frame.take().expect("made above");
        let pair = &flow_gpu.pairs[index];
        let params = Params { src_w: width, src_h: height, lr: luma[0], lg: luma[1], lb: luma[2], t, total: pair.work.0 * pair.work.1, ..Params::level(pair.work.0, pair.work.1, 0) };
        pass(&device, &mut enc, &flow_gpu.synth, &params, &[&pair.a, &pair.b, &pair.forward, &pair.backward, &pair.count, &out], (width, height));
        // The frame and its pair's count, read back in one map.
        enc.copy_buffer_to_buffer(&out, 0, &staging, 0, frame_bytes);
        enc.copy_buffer_to_buffer(&pair.count, 0, &staging, frame_bytes, 4);
        self.submit_now(enc);
        let slice = staging.slice(..frame_bytes + 4);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let mut readback = self.readbacks.begin(frame_bytes);
        if !matches!(poll_until_mapped(&device, &rx), Ok(Ok(()))) {
            self.mark_device_lost("flow readback failed");
            return None;
        }
        readback.succeeded = true;
        drop(readback);
        let data = slice.get_mapped_range();
        let pixels_out = super::mapped_bytes_to_f32(&data[..frame_bytes as usize], pixels * 4);
        let count = u32::from_le_bytes(data[frame_bytes as usize..frame_bytes as usize + 4].try_into().expect("four bytes"));
        drop(data);
        staging.unmap();
        let flow_gpu = self.flow_gpu.as_mut().expect("made above");
        flow_gpu.frame = Some((kept_bytes, out, staging));
        let pair = &mut flow_gpu.pairs[index];
        let unreliable = *pair.unreliable.get_or_insert(count as f32 / (pair.work.0 * pair.work.1) as f32);
        Some(Interpolated { pixels: pixels_out, unreliable, cross_faded: unreliable > flow::UNRELIABLE_FRACTION })
    }

    /// The source pairs whose flow is kept now, for a test.
    #[doc(hidden)]
    pub fn flow_pairs_kept(&self) -> usize {
        self.flow_gpu.as_ref().map_or(0, |f| f.pairs.len())
    }
}

/// Upload `a` and `b` and record their flow's estimate into `enc`.
#[allow(clippy::too_many_arguments)]
fn estimate_pair(device: &wgpu::Device, queue: &wgpu::Queue, gpu: &FlowGpu, enc: &mut wgpu::CommandEncoder, key: FlowKey, a: &[f32], b: &[f32], size: (u32, u32), luma: [f32; 3]) -> Pair {
    let sizes = flow::level_sizes(flow::work_size(size.0, size.1));
    let work = sizes[0];
    let offsets: Vec<u32> = sizes.iter().scan(0u32, |at, &(w, h)| {
        let here = *at;
        *at += w * h;
        Some(here)
    }).collect();
    let cells: u64 = sizes.iter().map(|&(w, h)| u64::from(w * h)).sum();
    let storage = |label: &str, size: u64, extra: wgpu::BufferUsages| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage: wgpu::BufferUsages::STORAGE | extra, mapped_at_creation: false });
    let frame_bytes = u64::from(size.0) * u64::from(size.1) * 16;
    let fa = storage("flow_a", frame_bytes, wgpu::BufferUsages::COPY_DST);
    let fb = storage("flow_b", frame_bytes, wgpu::BufferUsages::COPY_DST);
    queue.write_buffer(&fa, 0, bytemuck::cast_slice(a));
    queue.write_buffer(&fb, 0, bytemuck::cast_slice(b));
    let lum_a = storage("flow_lum_a", cells * 4, wgpu::BufferUsages::empty());
    let lum_b = storage("flow_lum_b", cells * 4, wgpu::BufferUsages::empty());
    let forward = storage("flow_forward", cells * 16, wgpu::BufferUsages::empty());
    let backward = storage("flow_backward", cells * 16, wgpu::BufferUsages::empty());
    let raw = storage("flow_raw", u64::from(work.0 * work.1) * 16, wgpu::BufferUsages::empty());
    let count = storage("flow_count", 4, wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC);
    queue.write_buffer(&count, 0, &[0; 4]);

    let base = Params { src_w: size.0, src_h: size.1, lr: luma[0], lg: luma[1], lb: luma[2], ..Params::level(work.0, work.1, 0) };
    pass(device, enc, &gpu.luma, &base, &[&fa, &lum_a], work);
    pass(device, enc, &gpu.luma, &base, &[&fb, &lum_b], work);
    for level in 1..sizes.len() {
        let (w, h) = sizes[level];
        let (cw, ch) = sizes[level - 1];
        let params = Params { cw, ch, coff: offsets[level - 1], ..Params::level(w, h, offsets[level]) };
        pass(device, enc, &gpu.halve, &params, &[&lum_a], (w, h));
        pass(device, enc, &gpu.halve, &params, &[&lum_b], (w, h));
    }
    for level in (0..sizes.len()).rev() {
        let (w, h) = sizes[level];
        let (cw, ch, coff) = if level + 1 < sizes.len() { (sizes[level + 1].0, sizes[level + 1].1, offsets[level + 1]) } else { (0, 0, 0) };
        let params = Params { cw, ch, coff, ..Params::level(w, h, offsets[level]) };
        for (from, to, field) in [(&lum_a, &lum_b, &forward), (&lum_b, &lum_a, &backward)] {
            pass(device, enc, &gpu.search, &params, &[from, to, field, &raw], (w, h));
            pass(device, enc, &gpu.smooth, &params, &[&raw, field], (w, h));
        }
    }
    pass(device, enc, &gpu.count, &Params::level(work.0, work.1, 0), &[&forward, &backward, &count], work);
    let bytes = 2 * frame_bytes + 2 * cells * 16;
    Pair { key, size, print: 0, a: fa, b: fb, forward, backward, count, work, unreliable: None, bytes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::tests::square_frame;

    const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn gpu() -> Option<GpuEffectsPipeline> {
        let gpu = GpuEffectsPipeline::new();
        if gpu.is_none() {
            eprintln!("no GPU adapter: skipped");
        }
        gpu
    }

    fn key(from: f64) -> FlowKey {
        FlowKey { clip: 7, from_secs: from, to_secs: from + 1.0 / 24.0 }
    }

    /// The GPU's frame against the CPU reference's: the largest
    /// difference and the share of values more than 0.02 apart.
    fn compare(gpu: &[f32], cpu: &[f32]) -> (f32, f32) {
        let worst = gpu.iter().zip(cpu).map(|(g, c)| (g - c).abs()).fold(0.0f32, f32::max);
        let off = gpu.iter().zip(cpu).filter(|(g, c)| (*g - *c).abs() > 0.02).count();
        (worst, off as f32 / cpu.len() as f32)
    }

    #[test]
    fn gpu_matches_the_cpu_reference() {
        let Some(mut gpu) = gpu() else { return };
        let (w, h) = (128, 96);
        let a = square_frame(w, h, 20.0, 30.0, 24.0);
        let b = square_frame(w, h, 36.0, 34.0, 24.0);
        for t in [0.25, 0.5, 0.75] {
            let on_gpu = gpu.interpolate_by_flow(key(0.0), &a, &b, w, h, t, LUMA).expect("drawn");
            let on_cpu = flow::interpolate(&a, &b, w, h, t, LUMA);
            assert_eq!(on_gpu.cross_faded, on_cpu.cross_faded);
            assert!((on_gpu.unreliable - on_cpu.unreliable).abs() < 0.01, "{} vs {}", on_gpu.unreliable, on_cpu.unreliable);
            let (worst, off) = compare(&on_gpu.pixels, &on_cpu.pixels);
            // Float rounding can tip a flat patch's tie: a few pixels
            // may land elsewhere, never many.
            assert!(off < 0.01, "t = {t}: {off} of values differ, worst {worst}");
        }
        // One pair, estimated once.
        assert_eq!(gpu.flow_pairs_kept(), 1);
    }

    #[test]
    fn gpu_warps_a_frame_wider_than_its_flow() {
        let Some(mut gpu) = gpu() else { return };
        let (w, h) = (1280, 96);
        let a = square_frame(w, h, 200.0, 30.0, 40.0);
        let b = square_frame(w, h, 224.0, 34.0, 40.0);
        let on_gpu = gpu.interpolate_by_flow(key(0.0), &a, &b, w, h, 0.5, LUMA).expect("drawn");
        let on_cpu = flow::interpolate(&a, &b, w, h, 0.5, LUMA);
        let (worst, off) = compare(&on_gpu.pixels, &on_cpu.pixels);
        assert!(off < 0.01, "{off} of values differ, worst {worst}");
    }

    #[test]
    fn gpu_cross_fades_unrelated_frames() {
        let Some(mut gpu) = gpu() else { return };
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 10.0, 16.0);
        let b: Vec<f32> = (0..w * h).flat_map(|i| {
            let v = ((i * 7919) % 13) as f32 / 13.0;
            [v, v, v, 1.0]
        }).collect();
        let mid = gpu.interpolate_by_flow(key(0.0), &a, &b, w, h, 0.25, LUMA).expect("drawn");
        assert!(mid.cross_faded, "unreliable share {}", mid.unreliable);
        for ((o, a), b) in mid.pixels.iter().zip(&a).zip(&b) {
            assert!((o - (a + (b - a) * 0.25)).abs() < 1e-5);
        }
    }

    #[test]
    fn a_kept_pair_is_estimated_afresh_when_its_frames_change() {
        let Some(mut gpu) = gpu() else { return };
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 10.0, 16.0);
        let b = square_frame(w, h, 18.0, 10.0, 16.0);
        let first = gpu.interpolate_by_flow(key(0.0), &a, &b, w, h, 0.5, LUMA).expect("drawn");
        // The same key, other frames (relinked, regraded): drawn from them,
        // the stale pair replaced, not kept beside them.
        let brighter: Vec<f32> = b.iter().map(|v| v * 1.5).collect();
        let second = gpu.interpolate_by_flow(key(0.0), &a, &brighter, w, h, 0.5, LUMA).expect("drawn");
        let on_cpu = flow::interpolate(&a, &brighter, w, h, 0.5, LUMA);
        let (worst, off) = compare(&second.pixels, &on_cpu.pixels);
        assert!(off < 0.01, "{off} of values differ from the new frames' draw, worst {worst}");
        assert!(compare(&second.pixels, &first.pixels).1 > 0.01, "the old frames' draw came back");
        assert_eq!(gpu.flow_pairs_kept(), 1);
    }

    #[test]
    fn kept_pairs_are_bounded_and_dropped_on_a_seek() {
        let Some(mut gpu) = gpu() else { return };
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 10.0, 16.0);
        let b = square_frame(w, h, 12.0, 10.0, 16.0);
        for i in 0..6 {
            gpu.interpolate_by_flow(key(f64::from(i) / 24.0), &a, &b, w, h, 0.5, LUMA).expect("drawn");
        }
        assert_eq!(gpu.flow_pairs_kept(), KEPT_PAIRS);
        // Far away: the near ones go.
        gpu.interpolate_by_flow(key(60.0), &a, &b, w, h, 0.5, LUMA).expect("drawn");
        assert_eq!(gpu.flow_pairs_kept(), 1);
    }
}
