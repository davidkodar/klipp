//! The planar tracker on the GPU (#2754): `crate::planar`'s backend, its
//! passes the mirrors in [`super::planar_wgsl`]. A frame's pyramid and a
//! region's template stay on the device; an alignment runs each level's
//! iterations as one submission, every step (the light, the robust cut,
//! the normal equations, the solve and the update of the map) on the
//! device, and reads back only the map at the level's end. The CPU keeps
//! the decisions: the round trip, the confidence, the states.
//!
//! It owns its pipelines only, on the shared device, so a track job off
//! the render thread doesn't build every effect's.

use std::sync::Arc;

use wgpu::util::DeviceExt;

use super::planar_wgsl::{self as w, sources};
use crate::homography::Homography;
use crate::planar::{self, Backend, Fit, Frame, Options, Tracked};
use crate::window_track::GrayFrame;

/// The parameters every pass takes.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    w: u32,
    h: u32,
    n: u32,
    wgs: u32,
    cx: f32,
    cy: f32,
    s: f32,
    k: f32,
    radius: i32,
    step: u32,
    shift_x: f32,
    shift_y: f32,
}
// 12 fields x 4 bytes = 48 bytes

/// The tracker's pipelines, by pass.
struct Pipes {
    luma: wgpu::ComputePipeline,
    halve_rows: wgpu::ComputePipeline,
    halve_cols: wgpu::ComputePipeline,
    edges: wgpu::ComputePipeline,
    template: wgpu::ComputePipeline,
    strong: wgpu::ComputePipeline,
    sample: wgpu::ComputePipeline,
    light_partial: wgpu::ComputePipeline,
    light_solve: wgpu::ComputePipeline,
    residual: wgpu::ComputePipeline,
    cut: wgpu::ComputePipeline,
    normal_partial: wgpu::ComputePipeline,
    step: wgpu::ComputePipeline,
    condition: wgpu::ComputePipeline,
    zncc: wgpu::ComputePipeline,
    search: wgpu::ComputePipeline,
}

/// The planar tracker's device and passes.
pub struct PlanarGpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pipes: Pipes,
}

/// One level of a frame's pyramid on the device: its two channels.
struct Level {
    w: u32,
    h: u32,
    c0: wgpu::Buffer,
    c1: wgpu::Buffer,
}

/// A frame's pyramid on the device, finest first.
pub struct GpuPyramid {
    levels: Vec<Level>,
}

/// One level of a template on the device: its points and each channel's
/// strong threshold.
struct TLevel {
    n: u32,
    points: wgpu::Buffer,
    strong: wgpu::Buffer,
}

/// A region of a frame on the device, ready to align others to.
pub struct GpuTemplate {
    frame: Frame,
    levels: Vec<TLevel>,
}

fn pipeline(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(label), layout: None, module: &module, entry_point: "main", compilation_options: Default::default(), cache: None })
}

/// Workgroups for `n` points of 64, split across two dimensions so no
/// dimension passes the limit; the shaders rebuild the index.
fn flat(n: u32) -> (u32, u32) {
    let groups = n.div_ceil(64).max(1);
    let x = groups.min(65_535);
    (x, groups.div_ceil(x))
}

impl PlanarGpu {
    /// The tracker on the shared device; `None` without one.
    pub fn new() -> Option<Self> {
        let ctx = crate::gpu_context::GpuContext::shared()?;
        let (device, queue) = (ctx.device(), ctx.queue());
        let mut built: std::collections::HashMap<&str, wgpu::ComputePipeline> = sources().into_iter().map(|(label, source)| (label, pipeline(&device, label, &source))).collect();
        let mut take = |label: &str| built.remove(label).expect("every planar pass is built");
        let pipes = Pipes {
            luma: take("planar_luma"),
            halve_rows: take("planar_halve_rows"),
            halve_cols: take("planar_halve_cols"),
            edges: take("planar_edges"),
            template: take("planar_template"),
            strong: take("planar_strong"),
            sample: take("planar_sample"),
            light_partial: take("planar_light_partial"),
            light_solve: take("planar_light_solve"),
            residual: take("planar_residual"),
            cut: take("planar_cut"),
            normal_partial: take("planar_normal_partial"),
            step: take("planar_step"),
            condition: take("planar_condition"),
            zncc: take("planar_zncc"),
            search: take("planar_search"),
        };
        Some(Self { device, queue, pipes })
    }

    /// Follow the surface `quad` of `reference` through `frames`, as
    /// [`crate::planar::track`] does, on the device.
    #[allow(clippy::too_many_arguments)]
    pub fn track(&mut self, reference: &GrayFrame, quad: [(f64, f64); 4], exclude: &[Vec<(f64, f64)>], frames: &[usize], next: &mut dyn FnMut(usize) -> Option<GrayFrame>, opts: &Options, progress: &dyn Fn(f32) -> bool) -> Vec<Tracked> {
        planar::track_on(self, reference, quad, exclude, frames, next, opts, progress)
    }

    fn storage(&self, label: &str, bytes: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false })
    }

    fn filled(&self, label: &str, contents: &[u8]) -> wgpu::Buffer {
        let mut bytes = contents.to_vec();
        bytes.resize(bytes.len().max(16).next_multiple_of(4), 0);
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: &bytes, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC })
    }

    /// `pipe`'s bindings: the parameters at 0, then `buffers` in order.
    fn group(&self, pipe: &wgpu::ComputePipeline, params: &Params, buffers: &[&wgpu::Buffer]) -> wgpu::BindGroup {
        let uniform = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("planar_params"), contents: bytemuck::bytes_of(params), usage: wgpu::BufferUsages::UNIFORM });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
        entries.extend(buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32 + 1, resource: b.as_entire_binding() }));
        self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("planar"), layout: &pipe.get_bind_group_layout(0), entries: &entries })
    }

    /// `bytes` of `src` read back after `enc` runs.
    fn read(&self, mut enc: wgpu::CommandEncoder, src: &wgpu::Buffer, bytes: u64) -> Option<Vec<f32>> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("planar_read"), size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        enc.copy_buffer_to_buffer(src, 0, &staging, 0, bytes);
        self.queue.submit(Some(enc.finish()));
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        if !matches!(super::poll_until_mapped(&self.device, &rx), Ok(Ok(()))) {
            return None;
        }
        let out = bytemuck::cast_slice::<u8, f32>(&slice.get_mapped_range()).to_vec();
        staging.unmap();
        Some(out)
    }

    fn encoder(&self) -> wgpu::CommandEncoder {
        self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("planar") })
    }
}

/// The map as the state holds it, row by row.
fn to_f32(g: &Homography) -> [f32; 9] {
    let h = &g.h;
    [h[0][0], h[0][1], h[0][2], h[1][0], h[1][1], h[1][2], h[2][0], h[2][1], h[2][2]].map(|v| v as f32)
}

fn from_f32(s: &[f32]) -> Homography {
    Homography { h: [[s[0] as f64, s[1] as f64, s[2] as f64], [s[3] as f64, s[4] as f64, s[5] as f64], [s[6] as f64, s[7] as f64, s[8] as f64]] }
}

impl Backend for PlanarGpu {
    type Pyr = GpuPyramid;
    type Tpl = GpuTemplate;

    fn pyramid(&mut self, frame: &GrayFrame, levels: usize) -> GpuPyramid {
        let sizes = planar::level_sizes(frame.width, frame.height, levels);
        let bytes = self.filled("planar_bytes", &frame.data);
        let mut enc = self.encoder();
        let mut out: Vec<Level> = Vec::with_capacity(levels);
        for (l, &(lw, lh)) in sizes.iter().enumerate() {
            let (lw, lh) = (lw as u32, lh as u32);
            let px = (lw as u64) * (lh as u64) * 4;
            let (c0, c1) = (self.storage("planar_c0", px), self.storage("planar_c1", px));
            let own = Params { w: lw, h: lh, ..Params::default() };
            if l == 0 {
                let g = self.group(&self.pipes.luma, &own, &[&bytes, &c0]);
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("planar_luma"), timestamp_writes: None });
                pass.set_pipeline(&self.pipes.luma);
                pass.set_bind_group(0, &g, &[]);
                pass.dispatch_workgroups(lw.div_ceil(8), lh.div_ceil(8), 1);
            } else {
                let below = &out[l - 1];
                let tmp = self.storage("planar_halved", (below.w as u64) * (lh as u64) * 4);
                let rows = self.group(&self.pipes.halve_rows, &Params { w: below.w, h: below.h, ..Params::default() }, &[&below.c0, &tmp]);
                let cols = self.group(&self.pipes.halve_cols, &Params { w: below.w, h: lh, ..Params::default() }, &[&tmp, &c0]);
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("planar_halve"), timestamp_writes: None });
                pass.set_pipeline(&self.pipes.halve_rows);
                pass.set_bind_group(0, &rows, &[]);
                pass.dispatch_workgroups(below.w.div_ceil(8), lh.div_ceil(8), 1);
                pass.set_pipeline(&self.pipes.halve_cols);
                pass.set_bind_group(0, &cols, &[]);
                pass.dispatch_workgroups(lw.div_ceil(8), lh.div_ceil(8), 1);
            }
            let g = self.group(&self.pipes.edges, &own, &[&c0, &c1]);
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("planar_edges"), timestamp_writes: None });
                pass.set_pipeline(&self.pipes.edges);
                pass.set_bind_group(0, &g, &[]);
                pass.dispatch_workgroups(lw.div_ceil(8), lh.div_ceil(8), 1);
            }
            out.push(Level { w: lw, h: lh, c0, c1 });
        }
        self.queue.submit(Some(enc.finish()));
        GpuPyramid { levels: out }
    }

    fn template(&mut self, pyr: &GpuPyramid, quad: &[(f64, f64); 4], exclude: &[Vec<(f64, f64)>], opts: &Options) -> GpuTemplate {
        let sizes: Vec<(usize, usize)> = pyr.levels.iter().map(|l| (l.w as usize, l.h as usize)).collect();
        let (frame, cells) = planar::cells(&sizes, quad, exclude, opts);
        let mut enc = self.encoder();
        let levels = cells
            .iter()
            .zip(&pyr.levels)
            .enumerate()
            .map(|(l, (cells, level))| {
                let n = cells.len() as u32;
                let flat_cells: Vec<i32> = cells.iter().flat_map(|&(x, y)| [x, y]).collect();
                let cells_buf = self.filled("planar_cells", bytemuck::cast_slice(&flat_cells));
                let points = self.storage("planar_points", n as u64 * w::POINT as u64 * 4);
                let hist = self.filled("planar_hist", &vec![0u8; 2 * w::BINS as usize * 4]);
                let strong = self.storage("planar_strong", 8);
                let k = 1.0 / (1u32 << l) as f32;
                let params = Params { w: level.w, h: level.h, n, cx: frame.cx as f32, cy: frame.cy as f32, s: frame.s as f32, k, ..Params::default() };
                let build = self.group(&self.pipes.template, &params, &[&cells_buf, &level.c0, &level.c1, &points, &hist]);
                let median = self.group(&self.pipes.strong, &params, &[&hist, &strong]);
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("planar_template"), timestamp_writes: None });
                if n > 0 {
                    let (gx, gy) = flat(n);
                    pass.set_pipeline(&self.pipes.template);
                    pass.set_bind_group(0, &build, &[]);
                    pass.dispatch_workgroups(gx, gy, 1);
                }
                pass.set_pipeline(&self.pipes.strong);
                pass.set_bind_group(0, &median, &[]);
                pass.dispatch_workgroups(1, 1, 1); // flat-ok: one thread finds the median
                TLevel { n, points, strong }
            })
            .collect();
        self.queue.submit(Some(enc.finish()));
        GpuTemplate { frame, levels }
    }

    fn region(&self, tpl: &GpuTemplate) -> Frame {
        tpl.frame
    }

    fn align(&mut self, tpl: &GpuTemplate, pyr: &GpuPyramid, mut g: Homography, top: usize, bottom: usize, search: i32, iterations: usize) -> Option<Fit> {
        let top = top.min(pyr.levels.len() - 1).min(tpl.levels.len() - 1);
        let most = tpl.levels[bottom..=top].iter().map(|t| t.n).max().unwrap_or(0).max(1) as u64;
        let state = self.storage("planar_state", w::STATE as u64 * 4);
        let seen = self.storage("planar_seen", most * 16);
        let res = self.storage("planar_res", most * 8);
        let trust = self.storage("planar_trust", most * 8);
        let hist = self.filled("planar_hist", &vec![0u8; w::BINS as usize * 4]);
        let partial = self.storage("planar_partial", w::GROUPS as u64 * w::NORMAL as u64 * 4);
        let mut conditioning = 0.0;
        for l in (bottom..=top).rev() {
            let (t, level) = (&tpl.levels[l], &pyr.levels[l]);
            if t.n < 16 {
                return None;
            }
            let mut gl = planar::to_level(l).multiply(&g);
            let params = Params { w: level.w, h: level.h, n: t.n, wgs: w::GROUPS, ..Params::default() };
            if l == top && search > 0 {
                gl = self.search_top(t, level, gl, search)?;
            }
            // The map to start from, the light as one, no failure; every
            // point trusted.
            let mut start = [0.0f32; w::STATE as usize];
            start[..9].copy_from_slice(&to_f32(&gl));
            start[9] = 1.0;
            start[13] = 1.0;
            self.queue.write_buffer(&state, 0, bytemuck::cast_slice(&start));
            self.queue.write_buffer(&trust, 0, bytemuck::cast_slice(&vec![1.0f32; t.n as usize * 2]));
            let p = &self.pipes;
            let groups = [
                (&p.sample, self.group(&p.sample, &params, &[&t.points, &level.c0, &level.c1, &state, &seen])),
                (&p.light_partial, self.group(&p.light_partial, &params, &[&t.points, &seen, &trust, &partial, &state])),
                (&p.light_solve, self.group(&p.light_solve, &params, &[&partial, &state])),
                (&p.residual, self.group(&p.residual, &params, &[&t.points, &seen, &state, &res, &hist, &t.strong])),
                (&p.cut, self.group(&p.cut, &params, &[&hist, &state])),
                (&p.normal_partial, self.group(&p.normal_partial, &params, &[&t.points, &seen, &res, &state, &trust, &partial])),
                (&p.step, self.group(&p.step, &params, &[&partial, &state])),
            ];
            let condition = self.group(&p.condition, &params, &[&partial, &state]);
            let (gx, gy) = flat(t.n);
            let mut enc = self.encoder();
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("planar_align"), timestamp_writes: None });
                for _ in 0..iterations {
                    for (k, (pipe, group)) in groups.iter().enumerate() {
                        pass.set_pipeline(pipe);
                        pass.set_bind_group(0, group, &[]);
                        match k {
                            0 | 3 => pass.dispatch_workgroups(gx, gy, 1),
                            1 | 5 => pass.dispatch_workgroups(w::GROUPS, 1, 1), // flat-ok: a fixed count of reduction groups
                            _ => pass.dispatch_workgroups(1, 1, 1),              // flat-ok: one thread solves
                        }
                    }
                }
                pass.set_pipeline(&p.condition);
                pass.set_bind_group(0, &condition, &[]);
                pass.dispatch_workgroups(1, 1, 1); // flat-ok: one thread, once a level
            }
            let s = self.read(enc, &state, w::STATE as u64 * 4)?;

            if s[19] > 0.5 || s[..9].iter().any(|v| !v.is_finite()) {
                return None;
            }
            conditioning = s[18] as f64;
            g = planar::from_level(l).multiply(&from_f32(&s[..9]));
        }
        Some(Fit { g, conditioning })
    }

    fn zncc(&mut self, tpl: &GpuTemplate, pyr: &GpuPyramid, g: &Homography, l: usize) -> f64 {
        let l = l.min(pyr.levels.len() - 1).min(tpl.levels.len() - 1);
        let (t, level) = (&tpl.levels[l], &pyr.levels[l]);
        let state = self.filled("planar_state", bytemuck::cast_slice(&to_f32(&planar::to_level(l).multiply(g))));
        let partial = self.storage("planar_zncc", w::GROUPS as u64 * 6 * 4);
        let params = Params { w: level.w, h: level.h, n: t.n, ..Params::default() };
        let group = self.group(&self.pipes.zncc, &params, &[&t.points, &level.c0, &level.c1, &state, &partial]);
        let mut enc = self.encoder();
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("planar_zncc"), timestamp_writes: None });
            pass.set_pipeline(&self.pipes.zncc);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(w::GROUPS, 1, 1); // flat-ok: a fixed count of reduction groups
        }
        let Some(p) = self.read(enc, &partial, w::GROUPS as u64 * 6 * 4) else { return 0.0 };
        let mut s = [0.0f64; 6];
        for g in p.chunks(6) {
            for (a, b) in s.iter_mut().zip(g) {
                *a += *b as f64;
            }
        }
        let n = s[0];
        if n < 16.0 {
            return 0.0;
        }
        let (ma, mb) = (s[1] / n, s[2] / n);
        let (aa, bb, ab) = (s[3] - n * ma * ma, s[4] - n * mb * mb, s[5] - n * ma * mb);
        if aa < 1e-9 || bb < 1e-9 { 0.0 } else { ab / (aa * bb).sqrt() }
    }
}

impl PlanarGpu {
    /// The top level searched `radius` pixels around `g` for the shift
    /// that matches best, as `planar::search_top` does.
    fn search_top(&self, t: &TLevel, level: &Level, g: Homography, radius: i32) -> Option<Homography> {
        let step = t.n.div_ceil(1500).max(1);
        let side = (2 * radius + 1) as u32;
        let state = self.filled("planar_state", bytemuck::cast_slice(&to_f32(&g)));
        let costs = self.storage("planar_costs", side as u64 * side as u64 * 8);
        let params = Params { w: level.w, h: level.h, n: t.n, radius, step, ..Params::default() };
        let group = self.group(&self.pipes.search, &params, &[&t.points, &level.c0, &level.c1, &state, &costs]);
        let mut enc = self.encoder();
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("planar_search"), timestamp_writes: None });
            pass.set_pipeline(&self.pipes.search);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(side * side, 1, 1); // flat-ok: one group per shift, at most 37 × 37
        }
        let c = self.read(enc, &costs, side as u64 * side as u64 * 8)?;
        let read = t.n.div_ceil(step) as f32;
        let cost = |i: usize| if c[2 * i + 1] * 2.0 < read { f64::MAX } else { (c[2 * i] / c[2 * i + 1]) as f64 };
        // Unshifted first, then each shift in turn, the first lowest kept.
        let at = |dx: i32, dy: i32| ((dy + radius) as u32 * side + (dx + radius) as u32) as usize;
        let mut best = (cost(at(0, 0)), 0, 0);
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let here = cost(at(dx, dy));
                if here < best.0 {
                    best = (here, dx, dy);
                }
            }
        }
        Some(Homography { h: [[1.0, 0.0, best.1 as f64], [0.0, 1.0, best.2 as f64], [0.0, 0.0, 1.0]] }.multiply(&g))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texture(x: f64, y: f64) -> u8 {
        (128.0 + 40.0 * (x * 0.21).sin() * (y * 0.17).cos() + 30.0 * (x * 0.05 + y * 0.07).sin() + 25.0 * ((x - y) * 0.11).cos()) as u8
    }

    fn frame(w: usize, h: usize, dx: f64) -> GrayFrame {
        GrayFrame { data: (0..h).flat_map(|y| (0..w).map(move |x| texture(x as f64 - dx, y as f64))).collect(), width: w, height: h }
    }

    impl PlanarGpu {
        fn channels(&self, pyr: &GpuPyramid) -> Vec<[Vec<f32>; 2]> {
            pyr.levels.iter().map(|l| {
                let px = (l.w as u64) * (l.h as u64) * 4;
                [self.read(self.encoder(), &l.c0, px).unwrap(), self.read(self.encoder(), &l.c1, px).unwrap()]
            }).collect()
        }
    }

    /// Stage by stage against the CPU: the pyramid, then an alignment of
    /// a frame moved two pixels.
    #[test]
    fn each_stage_agrees_with_the_cpus() {
        let Some(mut g) = PlanarGpu::new() else {
            assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
            return;
        };
        let (w, h) = (161, 97);
        let f = frame(w, h, 0.0);
        let cpu = planar::Cpu.pyramid(&f, 3);
        let gpu = g.pyramid(&f, 3);
        for (l, chans) in g.channels(&gpu).iter().enumerate() {
            for c in 0..2 {
                let want = planar::cpu_channel(&cpu, l, c);
                assert_eq!(chans[c].len(), want.len(), "level {l} channel {c}");
                let worst = chans[c].iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
                assert!(worst < 1e-5, "level {l} channel {c}: {worst}");
            }
        }
        let quad = [(40.0, 30.0), (120.0, 30.0), (120.0, 70.0), (40.0, 70.0)];
        let opts = Options::default();
        let tc = planar::Cpu.template(&cpu, &quad, &[], &opts);
        let tg = g.template(&gpu, &quad, &[], &opts);
        let moved = frame(w, h, 2.0);
        let (pc, pg) = (planar::Cpu.pyramid(&moved, 3), g.pyramid(&moved, 3));
        let start = planar::Cpu.region(&tc).px();
        let a = planar::Cpu.align(&tc, &pc, start, 2, 0, 0, 25).expect("the CPU aligns");
        let b = g.align(&tg, &pg, start, 2, 0, 0, 25).expect("the GPU aligns");
        let at = |h: &Homography| h.transform(0.0, 0.0);
        assert!((at(&a.g).0 - at(&b.g).0).abs() < 0.02 && (at(&a.g).1 - at(&b.g).1).abs() < 0.02, "{:?} vs {:?}", at(&a.g), at(&b.g));
    }
}
