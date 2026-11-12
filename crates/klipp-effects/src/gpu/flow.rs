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
//!
//! On the executor's chain (#2854) the frames are already on the GPU:
//! a pair's flow is estimated from them there and only its fields are
//! kept, each in-between drawn from the frames bound as they are, into
//! the chain's buffer, nothing read back. Its key alone names the pair
//! there; whether its motion was followed is read from its count without
//! waiting, by the frame after.
use super::GpuEffectsPipeline;
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
    /// The two frames, uploaded; `None` for a pair of the chain's, whose
    /// frames are bound as they are each time.
    frames: Option<(wgpu::Buffer, wgpu::Buffer)>,
    /// A chain pair's count, copied out to be read without waiting, and
    /// what the read found.
    count_read: Option<CountRead>,
    forward: wgpu::Buffer,
    backward: wgpu::Buffer,
    count: wgpu::Buffer,
    work: (u32, u32),
    /// Read back with the pair's first frame; kept after.
    unreliable: Option<f32>,
    bytes: u64,
}

/// A pair's count on its way back without a wait (#2854).
struct CountRead {
    staging: wgpu::Buffer,
    /// The copy into it is submitted: the pair was made on an earlier
    /// call, whose chain has gone.
    submitted: bool,
    asked: bool,
    /// Whether the map came back, and well, once it has.
    mapped: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
}

/// The flow's six compute pipelines, compiled once for a device and
/// shared by every renderer on it ([`pipelines_for`]).
pub(super) struct FlowPipelines {
    luma: wgpu::ComputePipeline,
    halve: wgpu::ComputePipeline,
    search: wgpu::ComputePipeline,
    smooth: wgpu::ComputePipeline,
    count: wgpu::ComputePipeline,
    synth: wgpu::ComputePipeline,
}

impl FlowPipelines {
    fn build(device: &wgpu::Device) -> Self {
        let [luma, halve, search, smooth, count, synth] = sources().map(|(label, source)| pipeline(device, label, &source));
        FlowPipelines { luma, halve, search, smooth, count, synth }
    }
}

/// A device's flow pipelines: being compiled, or done.
struct Warm {
    ready: std::sync::Mutex<WarmState>,
    done: std::sync::Condvar,
    /// Compiled by the first flow frame itself: nothing warmed them.
    built_on_use: std::sync::atomic::AtomicBool,
}

/// Each device's warm-up, while the device lives.
static KEPT: std::sync::Mutex<Vec<(std::sync::Weak<wgpu::Device>, std::sync::Arc<Warm>)>> = std::sync::Mutex::new(Vec::new());

/// The device's entry, made when there was none (`true`).
fn warm_entry(device: &std::sync::Arc<wgpu::Device>) -> (std::sync::Arc<Warm>, bool) {
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.retain(|(d, _)| d.strong_count() > 0);
    if let Some((_, w)) = kept.iter().find(|(d, _)| std::ptr::eq(d.as_ptr(), std::sync::Arc::as_ptr(device))) {
        return (w.clone(), false);
    }
    let w = std::sync::Arc::new(Warm { ready: std::sync::Mutex::new(WarmState::Compiling), done: std::sync::Condvar::new(), built_on_use: std::sync::atomic::AtomicBool::new(false) });
    kept.push((std::sync::Arc::downgrade(device), w.clone()));
    (w, true)
}

/// Where a device's warm-up is.
enum WarmState {
    Compiling,
    Done(std::sync::Arc<FlowPipelines>),
    /// The warm-up's thread panicked: whoever asks compiles them.
    Failed,
}

fn finish(w: &Warm, built: FlowPipelines) -> std::sync::Arc<FlowPipelines> {
    let built = std::sync::Arc::new(built);
    *w.ready.lock().unwrap_or_else(|e| e.into_inner()) = WarmState::Done(built.clone());
    w.done.notify_all();
    built
}

/// Devices whose warm-up is made to panic, for tests.
#[cfg(test)]
static WARM_FAILS_FOR: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Devices whose warm-up is made to take this long, for tests.
#[cfg(test)]
static WARM_SLOW_FOR: std::sync::Mutex<Vec<(usize, std::time::Duration)>> = std::sync::Mutex::new(Vec::new());

/// Whether a sequence that draws by optical flow is open ([`want_flow`]):
/// until then no device warms the flow's pipelines.
static FLOW_WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Devices made while the flow was not wanted, warmed if it comes to be.
static UNWARMED: std::sync::Mutex<Vec<std::sync::Weak<wgpu::Device>>> = std::sync::Mutex::new(Vec::new());

/// A renderer was made on `device`: its flow pipelines are warmed now if
/// a sequence needs them, else kept in mind for when one does. Warmed on
/// every device, the compile ran in every process that drew anything, an
/// export of a few frames too, and a quit in the middle of it crashed
/// NVIDIA's driver (`NVVM compilation failed`, then SIGSEGV at exit).
pub(super) fn on_new_device(device: &std::sync::Arc<wgpu::Device>, wanted: bool) {
    if wanted {
        warm(device);
    } else {
        let mut unwarmed = UNWARMED.lock().unwrap_or_else(|e| e.into_inner());
        unwarmed.retain(|d| d.strong_count() > 0);
        unwarmed.push(std::sync::Arc::downgrade(device));
    }
}

/// Whether the flow is wanted in this process.
pub fn flow_wanted() -> bool {
    FLOW_WANTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// A sequence draws by optical flow: warm the flow's pipelines on every
/// device, now and from here on. Once is enough.
pub fn want_flow() {
    if !FLOW_WANTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        warm_unwarmed();
    }
}

/// Warm every device made while the flow was not wanted.
fn warm_unwarmed() {
    let devices: Vec<_> = std::mem::take(&mut *UNWARMED.lock().unwrap_or_else(|e| e.into_inner())).into_iter().filter_map(|d| d.upgrade()).collect();
    for device in devices {
        warm(&device);
    }
}

/// Whether `device`'s flow pipelines have been asked for, by a warm-up
/// or a flow frame.
#[cfg(test)]
fn warm_started(device: &std::sync::Arc<wgpu::Device>) -> bool {
    KEPT.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(d, _)| std::ptr::eq(d.as_ptr(), std::sync::Arc::as_ptr(device)))
}

/// Wait, at most `bound`, for every warm-up still compiling: what a
/// process does before it exits, since a driver can crash when its
/// compiler is torn down under a compile. `true` when none is left.
pub fn settle_flow_warmups(bound: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + bound;
    let warms: Vec<std::sync::Arc<Warm>> = KEPT.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, w)| w.clone()).collect();
    for w in warms {
        let mut ready = w.ready.lock().unwrap_or_else(|e| e.into_inner());
        while matches!(*ready, WarmState::Compiling) {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            ready = w.done.wait_timeout(ready, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
    true
}

/// The longest a flow frame waits for a warm-up still compiling before it
/// compiles the pipelines itself.
const WARM_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Start compiling the flow's pipelines for `device` on a thread of their
/// own, once per device. Compiled on a speed ramp's first flow frame, the
/// six took about 90 ms of that frame.
pub(super) fn warm(device: &std::sync::Arc<wgpu::Device>) {
    let (w, new) = warm_entry(device);
    if !new {
        return;
    }
    let d = device.clone();
    let wt = w.clone();
    let spawned = std::thread::Builder::new().name("flow-pipelines".into()).spawn(move || {
        // A compile that panics (a device lost, an error the device's
        // handler raises) must not leave a flow frame waiting for ever.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if WARM_FAILS_FOR.lock().unwrap_or_else(|e| e.into_inner()).contains(&(std::sync::Arc::as_ptr(&d) as usize)) {
                panic!("a warm-up made to fail, for a test");
            }
            #[cfg(test)]
            {
                // Read, then slept on with the list let go: held, it held
                // every other device's warm-up too.
                let slow = WARM_SLOW_FOR.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(id, _)| *id == std::sync::Arc::as_ptr(&d) as usize).map(|(_, slow)| *slow);
                if let Some(slow) = slow {
                    std::thread::sleep(slow);
                }
            }
            FlowPipelines::build(&d)
        })) {
            Ok(built) => {
                finish(&wt, built);
            }
            Err(_) => {
                tracing::warn!("flow pipelines: their compile panicked; a flow frame compiles them itself");
                *wt.ready.lock().unwrap_or_else(|e| e.into_inner()) = WarmState::Failed;
                wt.done.notify_all();
            }
        }
    });
    if let Err(e) = spawned {
        // No thread: compiled here, as before, once.
        tracing::warn!("flow pipelines: could not compile them on a thread of their own: {e}");
        finish(&w, FlowPipelines::build(device));
    }
}

/// `device`'s flow pipelines: the warmed ones, waiting only for what is
/// left of their compile; compiled here when nothing warmed them.
pub(super) fn pipelines_for(device: &std::sync::Arc<wgpu::Device>) -> std::sync::Arc<FlowPipelines> {
    let (w, new) = warm_entry(device);
    if new {
        w.built_on_use.store(true, std::sync::atomic::Ordering::SeqCst);
        return finish(&w, FlowPipelines::build(device));
    }
    let deadline = std::time::Instant::now() + WARM_WAIT;
    let mut ready = w.ready.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        match &*ready {
            WarmState::Done(p) => return p.clone(),
            WarmState::Failed => break,
            WarmState::Compiling => {}
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            tracing::warn!("flow pipelines: their warm-up has not finished in {WARM_WAIT:?}; compiled here");
            break;
        }
        ready = w.done.wait_timeout(ready, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
    }
    // Compiled here, as before the warm-up, and kept for the others.
    drop(ready);
    w.built_on_use.store(true, std::sync::atomic::Ordering::SeqCst);
    finish(&w, FlowPipelines::build(device))
}

pub(super) struct FlowGpu {
    pipelines: std::sync::Arc<FlowPipelines>,
    pairs: Vec<Pair>,
    /// The drawn frame's buffer and its read-back copy (with room for the
    /// count after it), kept for the next frame of the same size.
    frame: Option<(u64, wgpu::Buffer, wgpu::Buffer)>,
    /// What it keeps, in the GPU memory report (#3174).
    gauge: crate::gpu_memory::Gauge,
}

impl FlowGpu {
    /// Lets go of its kept pairs and drawn frame (#3174): the GPU ran out
    /// of memory.
    pub(super) fn let_go(&mut self) {
        self.pairs.clear();
        self.frame = None;
    }

    /// What its kept pairs and drawn frame hold, reported (#3174).
    pub(super) fn report_memory(&mut self) {
        let pairs: u64 = self.pairs.iter().map(|p| p.bytes).sum();
        let frame = self.frame.as_ref().map_or(0, |(_, a, b)| a.size() + b.size());
        let n = self.pairs.len() as u64;
        self.gauge.report_holding(pairs + frame, crate::gpu_memory::Holding { count: n, in_use: 0, each: pairs.checked_div(n).unwrap_or(0), bytes_per_pixel: 0 });
    }
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
    prelude() + gpu_shader!("flow/luma.1.wgsl")
}

/// `halve`: the level at `off` from the one above it at `coff`.
fn shader_halve() -> String {
    prelude() + gpu_shader!("flow/halve.1.wgsl") + &plane_fns("up", "lum", "p.coff", "p.cw", "p.ch") + gpu_shader!("flow/halve.2.wgsl")
}

/// `search`: one level's raw field, written from index 0 of `raw`.
fn shader_search() -> String {
    prelude() + gpu_shader!("flow/search.1.wgsl") + &plane_fns("from", "from_l", "p.off", "p.w", "p.h") + &plane_fns("to", "to_l", "p.off", "p.w", "p.h") + &field_fns("coarse", "coarse", "p.coff", "p.cw", "p.ch") + gpu_shader!("flow/search.2.wgsl")
}

/// `smooth`: the level's field at `off` from the raw one.
fn shader_smooth() -> String {
    prelude() + gpu_shader!("flow/smooth.1.wgsl") + &field_fns("raw", "raw", "0u", "p.w", "p.h") + gpu_shader!("flow/smooth.2.wgsl")
}

/// `unreliable_share`'s count, at level 0.
fn shader_count() -> String {
    prelude() + gpu_shader!("flow/count.1.wgsl") + &field_fns("fwd", "fwd", "0u", "p.w", "p.h") + &field_fns("bwd", "bwd", "0u", "p.w", "p.h") + gpu_shader!("flow/count.2.wgsl")
}

/// `synthesise`: the in-between frame at `p.t`, at the frames' size
/// (`src_w` × `src_h`) from the flow at the work size (`w` × `h`).
fn shader_synth() -> String {
    prelude() + gpu_shader!("flow/synth.1.wgsl") + &field_fns("fwd", "fwd", "0u", "p.w", "p.h") + &field_fns("bwd", "bwd", "0u", "p.w", "p.h") + gpu_shader!("flow/synth.2.wgsl")
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

impl std::ops::Deref for FlowGpu {
    type Target = FlowPipelines;
    fn deref(&self) -> &FlowPipelines {
        &self.pipelines
    }
}

impl FlowGpu {
    pub(super) fn new(device: &std::sync::Arc<wgpu::Device>) -> Self {
        FlowGpu {
            pipelines: pipelines_for(device),
            pairs: Vec::new(),
            frame: None,
            gauge: crate::gpu_memory::Gauge::new(crate::gpu_memory::Held::FlowPairs),
        }
    }
}

/// One compute pass of `pipeline` over `w` × `h`, its bindings in order
/// from 0, the first `params`.
fn pass(device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, pipeline: &wgpu::ComputePipeline, params: &Params, buffers: &[&wgpu::Buffer], (w, h): (u32, u32)) {
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("flow_params"), contents: bytemuck::bytes_of(params), usage: wgpu::BufferUsages::UNIFORM });
    crate::gpu_memory::made(crate::gpu_memory::Made::FlowUniforms, uniform.size());
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
        let index = match flow_gpu.pairs.iter().position(|p| p.key == key && p.size == (width, height) && p.print == print && p.frames.is_some()) {
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
        let (fa, fb) = pair.frames.as_ref().expect("an uploaded pair");
        pass(&device, &mut enc, &flow_gpu.pipelines.synth, &params, &[fa, fb, &pair.forward, &pair.backward, &pair.count, &out], (width, height));
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
        if !self.wait_mapped(&staging, &rx, "flow readback") {
            self.flow_gpu.as_mut().expect("made above").frame = Some((kept_bytes, out, staging));
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

    /// The in-between at `t` from `a` to `b`, frames on the GPU already
    /// (linear RGBA, `width` × `height`), drawn into `out`, recorded on
    /// the chain's encoder (#2854): the chain's [`interpolate_by_flow`],
    /// nothing uploaded or read back. A frame whose motion can't be
    /// followed is cross-faded in the draw. `None` when it couldn't be
    /// recorded; else the share of the pair's frame that couldn't be
    /// followed once it is known, which is from the frame after the
    /// pair's first.
    ///
    /// [`interpolate_by_flow`]: Self::interpolate_by_flow
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_flow_on_chain(&mut self, key: FlowKey, a: &wgpu::Buffer, b: &wgpu::Buffer, out: &wgpu::Buffer, width: u32, height: u32, t: f32, luma: [f32; 3]) -> Option<Option<f32>> {
        let frame_bytes = u64::from(width) * u64::from(height) * 16;
        if self.is_device_lost() || width == 0 || height == 0 || [a, b, out].iter().any(|buf| buf.size() < frame_bytes) {
            return None;
        }
        let device = self.device.clone();
        let flow_gpu = self.flow_gpu.get_or_insert_with(|| FlowGpu::new(&device));
        let enc = self.chain_encoder.as_mut()?;
        let index = match flow_gpu.pairs.iter().position(|p| p.key == key && p.size == (width, height) && p.frames.is_none()) {
            Some(i) => i,
            None => {
                flow_gpu.pairs.retain(|p| p.key.clip == key.clip && p.key != key && (p.key.from_secs - key.from_secs).abs() <= KEPT_SPAN_SECS);
                let mut pair = estimate_fields(&device, &self.queue, flow_gpu, enc, key, a, b, (width, height), luma);
                let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("flow_count_read"), size: 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                enc.copy_buffer_to_buffer(&pair.count, 0, &staging, 0, 4);
                pair.count_read = Some(CountRead { staging, submitted: false, asked: false, mapped: Default::default() });
                flow_gpu.pairs.push(pair);
                while flow_gpu.pairs.len() > KEPT_PAIRS || (flow_gpu.pairs.len() > 1 && flow_gpu.pairs.iter().map(|p| p.bytes).sum::<u64>() > KEPT_BYTES) {
                    flow_gpu.pairs.remove(0);
                }
                flow_gpu.pairs.len() - 1
            }
        };
        let pair = &mut flow_gpu.pairs[index];
        let params = Params { src_w: width, src_h: height, lr: luma[0], lg: luma[1], lb: luma[2], t, total: pair.work.0 * pair.work.1, ..Params::level(pair.work.0, pair.work.1, 0) };
        pass(&device, enc, &flow_gpu.pipelines.synth, &params, &[a, b, &pair.forward, &pair.backward, &pair.count, out], (width, height));
        // The count, copied out with the estimate and submitted with it:
        // asked for by the frame after, once that chain has gone, and read
        // when it is there.
        if pair.unreliable.is_none() {
            if let Some(read) = &mut pair.count_read {
                if !read.submitted {
                    // Made on this call: its copy goes with this chain.
                    read.submitted = true;
                } else if read.asked {
                    let _ = device.poll(wgpu::Maintain::Poll);
                    let mapped = *read.mapped.lock().unwrap_or_else(|e| e.into_inner());
                    match mapped {
                        Some(true) => {
                            let count = u32::from_le_bytes(read.staging.slice(..4).get_mapped_range()[..4].try_into().expect("four bytes"));
                            read.staging.unmap();
                            pair.unreliable = Some(count as f32 / (pair.work.0 * pair.work.1) as f32);
                            pair.count_read = None;
                        }
                        // Not read: not known, and not asked again.
                        Some(false) => pair.count_read = None,
                        None => {}
                    }
                } else {
                    read.asked = true;
                    let mapped = std::sync::Arc::clone(&read.mapped);
                    read.staging.slice(..4).map_async(wgpu::MapMode::Read, move |r| {
                        *mapped.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.is_ok());
                    });
                }
            }
        }
        Some(pair.unreliable)
    }
}

/// Upload `a` and `b` and record their flow's estimate into `enc`.
#[allow(clippy::too_many_arguments)]
fn estimate_pair(device: &wgpu::Device, queue: &wgpu::Queue, gpu: &FlowGpu, enc: &mut wgpu::CommandEncoder, key: FlowKey, a: &[f32], b: &[f32], size: (u32, u32), luma: [f32; 3]) -> Pair {
    let storage = |label: &str, size: u64, extra: wgpu::BufferUsages| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage: wgpu::BufferUsages::STORAGE | extra, mapped_at_creation: false });
    let frame_bytes = u64::from(size.0) * u64::from(size.1) * 16;
    let fa = storage("flow_a", frame_bytes, wgpu::BufferUsages::COPY_DST);
    let fb = storage("flow_b", frame_bytes, wgpu::BufferUsages::COPY_DST);
    queue.write_buffer(&fa, 0, bytemuck::cast_slice(a));
    queue.write_buffer(&fb, 0, bytemuck::cast_slice(b));
    crate::gpu_memory::made(crate::gpu_memory::Made::FlowPair, 2 * frame_bytes);
    let mut pair = estimate_fields(device, queue, gpu, enc, key, &fa, &fb, size, luma);
    pair.bytes += 2 * frame_bytes;
    pair.frames = Some((fa, fb));
    pair
}

/// Record the flow's estimate between `fa` and `fb` (linear RGBA on the
/// GPU, `size`) into `enc`: a pair holding only its fields.
#[allow(clippy::too_many_arguments)]
fn estimate_fields(device: &wgpu::Device, queue: &wgpu::Queue, gpu: &FlowGpu, enc: &mut wgpu::CommandEncoder, key: FlowKey, fa: &wgpu::Buffer, fb: &wgpu::Buffer, size: (u32, u32), luma: [f32; 3]) -> Pair {
    let sizes = flow::level_sizes(flow::work_size(size.0, size.1));
    let work = sizes[0];
    let offsets: Vec<u32> = sizes.iter().scan(0u32, |at, &(w, h)| {
        let here = *at;
        *at += w * h;
        Some(here)
    }).collect();
    let cells: u64 = sizes.iter().map(|&(w, h)| u64::from(w * h)).sum();
    let storage = |label: &str, size: u64, extra: wgpu::BufferUsages| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage: wgpu::BufferUsages::STORAGE | extra, mapped_at_creation: false });
    let lum_a = storage("flow_lum_a", cells * 4, wgpu::BufferUsages::empty());
    let lum_b = storage("flow_lum_b", cells * 4, wgpu::BufferUsages::empty());
    let forward = storage("flow_forward", cells * 16, wgpu::BufferUsages::empty());
    let backward = storage("flow_backward", cells * 16, wgpu::BufferUsages::empty());
    let raw = storage("flow_raw", u64::from(work.0 * work.1) * 16, wgpu::BufferUsages::empty());
    let count = storage("flow_count", 4, wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC);
    queue.write_buffer(&count, 0, &[0; 4]);
    crate::gpu_memory::made(crate::gpu_memory::Made::FlowPair, [&lum_a, &lum_b, &forward, &backward, &raw, &count].iter().map(|b| b.size()).sum());

    let base = Params { src_w: size.0, src_h: size.1, lr: luma[0], lg: luma[1], lb: luma[2], ..Params::level(work.0, work.1, 0) };
    pass(device, enc, &gpu.luma, &base, &[fa, &lum_a], work);
    pass(device, enc, &gpu.luma, &base, &[fb, &lum_b], work);
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
    Pair { key, size, print: 0, frames: None, count_read: None, forward, backward, count, work, unreliable: None, bytes: 2 * cells * 16 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::tests::square_frame;

    const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

    /// #3174: when the GPU runs out of memory, the next run's end lets go
    /// of what the caches keep for reuse (the stack's layers, the flow
    /// pairs, the neighbours kept), and the GPU memory report says so.
    #[test]
    fn running_out_of_memory_trims_the_caches() {
        let Some(mut gpu) = gpu() else { return };
        let held = |name: &str| crate::gpu_memory::snapshot().held.into_iter().find(|h| h.name == name).map_or(0, |h| h.bytes);
        let make = |gpu: &GpuEffectsPipeline| std::sync::Arc::new(gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("test_layer"), size: 4096, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false }));
        gpu.stack_buf_size = 4096;
        gpu.stack_layers = vec![make(&gpu), make(&gpu)];
        gpu.stack_acc = Some(make(&gpu));
        let neighbour = gpu.neighbour_buffer(4096);
        drop(neighbour);
        gpu.end_slot_run();
        assert!(held("stack_layers") >= 3 * 4096 && held("neighbours") >= 4096, "kept before");
        crate::gpu_context::note_memory_pressure();
        gpu.end_slot_run();
        assert!(gpu.stack_layers.is_empty() && gpu.stack_acc.is_none() && gpu.flow_gpu.as_ref().is_none_or(|f| f.pairs.is_empty()));
        assert_eq!(gpu.neighbour_pool.lock().unwrap().kept(), 0, "the neighbours nothing holds let go");
        gpu.end_slot_run();
        assert_eq!(gpu.stack_layers.len(), 0, "a run without new pressure keeps what it has");
    }

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

    /// A device's flow pipelines, warmed on a thread of their own once a
    /// sequence needs them, are not compiled by the first flow frame:
    /// compiled there, they took about 90 ms of a speed ramp's first frame.
    #[test]
    fn the_first_flow_frame_compiles_no_pipelines() {
        // A device of its own: the others' flow frames, none of them
        // warmed, compile on theirs.
        let Some(ctx) = crate::gpu_context::GpuContext::new() else { return };
        warm(&ctx.device);
        let Some(mut gpu) = GpuEffectsPipeline::with_context(&ctx) else { return };
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 12.0, 12.0);
        let b = square_frame(w, h, 18.0, 14.0, 12.0);
        gpu.interpolate_by_flow(key(0.0), &a, &b, w, h, 0.5, LUMA).expect("drawn");
        let (warm, new) = warm_entry(&gpu.device);
        assert!(!new, "the device's pipelines are known");
        assert!(!warm.built_on_use.load(std::sync::atomic::Ordering::SeqCst), "the first flow frame compiled the pipelines");
        // A second renderer on the device shares them, compiling nothing.
        let first = pipelines_for(&gpu.device);
        assert!(std::sync::Arc::ptr_eq(&first, &pipelines_for(&gpu.device)));
    }

    /// A warm-up that fails leaves no flow frame waiting: the first one
    /// compiles the pipelines itself and is drawn, at once.
    #[test]
    fn a_failed_warm_up_does_not_hold_a_flow_frame() {
        let Some(ctx) = crate::gpu_context::GpuContext::new() else { return };
        WARM_FAILS_FOR.lock().unwrap().push(std::sync::Arc::as_ptr(&ctx.device) as usize);
        let Some(mut gpu) = GpuEffectsPipeline::with_context(&ctx) else { return };
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 12.0, 12.0);
        let b = square_frame(w, h, 18.0, 14.0, 12.0);
        let t0 = std::time::Instant::now();
        let drawn = gpu.interpolate_by_flow(key(0.0), &a, &b, w, h, 0.5, LUMA);
        assert!(drawn.is_some(), "the frame is drawn");
        assert!(t0.elapsed() < WARM_WAIT, "it did not wait out the bound: {:?}", t0.elapsed());
        let (warm, _) = warm_entry(&gpu.device);
        assert!(warm.built_on_use.load(std::sync::atomic::Ordering::SeqCst), "compiled by the frame, the warm-up having failed");
    }

    /// No warm-up starts on a device until a sequence wants the flow:
    /// then the devices already made are warmed.
    #[test]
    fn no_warm_up_starts_until_the_flow_is_wanted() {
        let Some(ctx) = crate::gpu_context::GpuContext::new() else { return };
        on_new_device(&ctx.device, false);
        assert!(!warm_started(&ctx.device), "a warm-up started with no flow wanted");
        warm_unwarmed();
        assert!(warm_started(&ctx.device), "wanted, the device made before is warmed");
        assert!(settle_flow_warmups(std::time::Duration::from_secs(60)), "and its warm-up ends");
    }

    /// An exit with a warm-up still compiling waits for it no longer than
    /// its bound.
    #[test]
    fn an_exit_waits_for_a_warm_up_only_as_long_as_its_bound() {
        let Some(ctx) = crate::gpu_context::GpuContext::new() else { return };
        WARM_SLOW_FOR.lock().unwrap().push((std::sync::Arc::as_ptr(&ctx.device) as usize, std::time::Duration::from_secs(3)));
        warm(&ctx.device);
        let t0 = std::time::Instant::now();
        assert!(!settle_flow_warmups(std::time::Duration::from_millis(300)), "a warm-up still compiling is said");
        let waited = t0.elapsed();
        assert!(waited >= std::time::Duration::from_millis(290) && waited < std::time::Duration::from_millis(600), "waited {waited:?}");
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
