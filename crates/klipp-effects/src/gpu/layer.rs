//! Resident layer effects. Every spill samples the unchanged layer, and
//! reverse src-over stacking keeps the first spill nearest the picture.
use super::record::EffectRecording;
use super::{BlurParams, GpuEffectsPipeline, WORKGROUP_SIZE};

pub(super) struct LayerPipelines {
    rows: wgpu::ComputePipeline,
    bounds: wgpu::ComputePipeline,
    corners: wgpu::ComputePipeline,
    cast: wgpu::ComputePipeline,
    over: wgpu::ComputePipeline,
    /// The recorded spills' passes (#2107): their bindings and a pipeline
    /// a pass.
    spill_layout: wgpu::BindGroupLayout,
    spill: [wgpu::ComputePipeline; SPILL_PASSES],
    fill_small: wgpu::ComputePipeline,
    fill_over: wgpu::ComputePipeline,
}

/// The layer passes on one renderer: the device's pipelines (#2688),
/// and the region, dispatch and scratch buffers this renderer writes.
pub(super) struct LayerGpu {
    pipelines: std::sync::Arc<LayerPipelines>,
    /// The region the recorded spills work in.
    spill_region: wgpu::Buffer,
    /// The region's indirect dispatch, apart from the region: a buffer a
    /// pass dispatches by is not one it may write.
    spill_args: wgpu::Buffer,
    /// Bound for the dispatch in the passes that don't write it.
    spill_unused: wgpu::Buffer,
    scratch: Vec<wgpu::Buffer>,
    size: u64,
}

impl LayerPipelines {
    pub(super) fn new(
        device: &wgpu::Device,
        single: &wgpu::PipelineLayout,
        pair: &wgpu::PipelineLayout,
    ) -> Self {
        let build = |layout: &wgpu::PipelineLayout, source: &str, entry: &str| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(entry),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(layout),
                module: &module,
                entry_point: entry,
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let spill_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("spill_layout"),
            entries: &[
                storage(0, true),
                storage(1, true),
                storage(2, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(4, false),
                storage(5, false),
            ],
        });
        let spill_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("spill_pl"),
            bind_group_layouts: &[&spill_layout],
            push_constant_ranges: &[],
        });
        Self {
            rows: build(single, ROWS, "rows"),
            bounds: build(single, BOUNDS, "bounds"),
            cast: build(single, CAST, "make_cast"),
            corners: build(pair, CORNERS, "corners"),
            over: build(pair, OVER, "over"),
            fill_small: build(pair, FILL_SMALL, "fill_small"),
            fill_over: build(pair, FILL_OVER, "fill_over"),
            spill: SPILL.map(|(_, source, entry)| build(&spill_pl, source, entry)),
            spill_layout,
        }
    }
}

impl LayerGpu {
    pub(super) fn new(device: &wgpu::Device, pipelines: std::sync::Arc<LayerPipelines>) -> Self {
        Self {
            pipelines,
            spill_region: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("spill_region"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }),
            spill_args: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("spill_args"),
                size: 48,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
                mapped_at_creation: false,
            }),
            spill_unused: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("spill_unused"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }),
            scratch: Vec::new(),
            size: 0,
        }
    }

    pub(super) fn ensure_buffers(&mut self, device: &wgpu::Device, size: u64) {
        if self.size >= size {
            return;
        }
        self.scratch = (0..2)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("layer_scratch"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        self.size = size;
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    radius: f32,
    feather: f32,
    offset_x: i32,
    offset_y: i32,
    opacity: f32,
    pad: u32,
    color: [f32; 4],
}

impl Params {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            radius: 0.0,
            feather: 0.0,
            offset_x: 0,
            offset_y: 0,
            opacity: 0.0,
            pad: 0,
            color: [0.0; 4],
        }
    }
}

/// The spills' parameters, as [`SPILL_HEAD`] reads them.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpillParams {
    width: u32,
    height: u32,
    radius: i32,
    /// [`SPILL_LAY`]: 0 for the first cast laid, which nothing is under.
    mode: u32,
    offset_x: i32,
    offset_y: i32,
    opacity: f32,
    pad: u32,
    /// How far the run's casts reach past the layer's bounds, left,
    /// right, up and down.
    reach: [i32; 4],
    color: [f32; 4],
}

/// The recorded spills' passes, in [`LayerGpu::spill`]'s order.
const SPILL_PASSES: usize = 8;
const RESET: usize = 0;
const BOUNDS_PASS: usize = 1;
const REGION: usize = 2;
const CAST_PASS: usize = 3;
const BLUR_H: usize = 4;
const BLUR_V: usize = 5;
const LAY: usize = 6;
const FINAL: usize = 7;

/// [`crate::layer::fill_bars`]'s parameters, as its two shaders read them.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FillParams {
    width: u32,
    height: u32,
    small_w: u32,
    small_h: u32,
    zoom: f32,
    darken: f32,
    pad: [u32; 2],
}

impl GpuEffectsPipeline {
    pub(super) fn layer_spills(
        &self,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        run: &[crate::layer::Spill],
    ) -> bool {
        let wg = (width * height).div_ceil(WORKGROUP_SIZE);
        let cast = self.buf_c.as_ref().unwrap();
        let mut under = &self.layer_gpu.scratch[0];
        let mut next = &self.layer_gpu.scratch[1];
        let mut drawn = false;
        for spill in run.iter().rev() {
            let mut params = Params::new(width, height);
            params.opacity = spill.opacity.clamp(0.0, 1.0);
            params.offset_x = (spill.offset_x * width as f32).round() as i32;
            params.offset_y = (spill.offset_y * width as f32).round() as i32;
            let sigma = spill.blur.max(0.0) * width as f32;
            if params.opacity <= 0.001
                || (params.offset_x == 0 && params.offset_y == 0 && sigma < 0.5)
            {
                continue;
            }
            params.color = [
                crate::cpu::decode(spill.color[0] as f32 / 255.0),
                crate::cpu::decode(spill.color[1] as f32 / 255.0),
                crate::cpu::decode(spill.color[2] as f32 / 255.0),
                0.0,
            ];
            self.write_params(&params);
            self.dispatch(&self.layer_gpu.pipelines.cast, input, cast, wg);
            if sigma >= 0.5 {
                for radius in crate::blur::boxes_for_gaussian(sigma) {
                    if radius == 0 {
                        continue;
                    }
                    self.write_params(&BlurParams {
                        width,
                        height,
                        radius: radius as i32,
                        _pad: 0,
                    });
                    self.dispatch(&self.pipelines.blur_h_pipeline, cast, output, wg);
                    self.dispatch(&self.pipelines.blur_v_pipeline, output, cast, wg);
                }
            }
            if drawn {
                self.write_params(&params);
                self.dispatch_pair(&self.layer_gpu.pipelines.over, cast, under, next, wg);
                std::mem::swap(&mut under, &mut next);
            } else {
                let mut encoder = self.encoder(Some("layer_first_spill"));
                encoder.copy_buffer_to_buffer(
                    cast,
                    0,
                    under,
                    0,
                    u64::from(width) * u64::from(height) * 16,
                );
                self.submit(encoder);
                drawn = true;
            }
        }
        if drawn {
            self.write_params(&Params::new(width, height));
            self.dispatch_pair(&self.layer_gpu.pipelines.over, input, under, output, wg);
        }
        drawn
    }
}

/// The recorded forms of the layer passes (node-graph M2 step 7): the
/// same passes in the same order, into the caller's encoder, each with a
/// uniform slot of its own. The caller's `scratch` stands where `buf_c`
/// stood, so a caller whose own frames live in `buf_c` is safe; the two
/// layer buffers are this pipeline's and only these passes touch them.
impl GpuEffectsPipeline {
    /// Whether the layer passes' own buffers hold a `width` × `height`
    /// frame: they are made by `ensure_recording_buffers`.
    pub(super) fn layer_ready(&self, width: u32, height: u32) -> bool {
        self.layer_gpu.scratch.len() == 2 && self.layer_gpu.size >= u64::from(width) * u64::from(height) * 16
    }

    /// Slots `record_layer_fill` claims at most.
    pub(super) const FILL_SLOTS: usize = 5;

    /// [`crate::layer::fill_bars`] on resident buffers, recorded: the
    /// picture's box found as the corners find it, the fill built an eighth
    /// the size, blurred there, and laid under the picture at full size
    /// (#680).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_layer_fill(
        &self,
        rec: &mut EffectRecording<'_>,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        bounds: &wgpu::Buffer,
        width: u32,
        height: u32,
        blur: f32,
        zoom: f32,
        darken: f32,
    ) {
        let q = &self.queue;
        let (sw, sh) = crate::layer::fill_size(width, height);
        let (small, spare) = (&self.layer_gpu.scratch[0], &self.layer_gpu.scratch[1]);
        let frame = rec.slots.claim(q, &Params::new(width, height)).expect("slots checked");
        self.record_pass_n(rec, &self.layer_gpu.pipelines.rows, src, spare, frame, height.div_ceil(WORKGROUP_SIZE));
        self.record_pass_n(rec, &self.layer_gpu.pipelines.bounds, spare, bounds, frame, 1);
        let fill = FillParams { width, height, small_w: sw, small_h: sh, zoom, darken: darken.clamp(0.0, 1.0), pad: [0; 2] };
        let fill = rec.slots.claim(q, &fill).expect("slots checked");
        self.record_pair_n(rec, &self.layer_gpu.pipelines.fill_small, src, bounds, small, fill, (sw * sh).div_ceil(WORKGROUP_SIZE));
        let sigma = blur.max(0.0) * sw as f32;
        if sigma >= 0.5 {
            for radius in crate::blur::boxes_for_gaussian(sigma) {
                if radius == 0 {
                    continue;
                }
                let slot = rec.slots.claim(q, &BlurParams { width: sw, height: sh, radius: radius as i32, _pad: 0 }).expect("slots checked");
                self.record_pass_n(rec, &self.pipelines.blur_h_pipeline, small, spare, slot, (sw * sh).div_ceil(WORKGROUP_SIZE));
                self.record_pass_n(rec, &self.pipelines.blur_v_pipeline, spare, small, slot, (sw * sh).div_ceil(WORKGROUP_SIZE));
            }
        }
        self.record_pair_n(rec, &self.layer_gpu.pipelines.fill_over, src, small, dst, fill, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    /// [`crate::layer::rounded_corners`] on resident buffers, recorded:
    /// one slot. One lane scans each row, then one lane reduces the row
    /// boxes; no frame download.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_layer_corners(
        &self,
        rec: &mut EffectRecording<'_>,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        bounds: &wgpu::Buffer,
        width: u32,
        height: u32,
        radius: f32,
        feather: f32,
    ) {
        let mut params = Params::new(width, height);
        params.radius = radius.clamp(0.0, 0.5);
        params.feather = feather.max(0.0);
        let slot = rec.slots.claim(&self.queue, &params).expect("slots checked");
        let rows = &self.layer_gpu.scratch[0];
        self.record_pass_n(rec, &self.layer_gpu.pipelines.rows, src, rows, slot, height.div_ceil(WORKGROUP_SIZE));
        self.record_pass_n(rec, &self.layer_gpu.pipelines.bounds, rows, bounds, slot, 1);
        self.record_pair_n(rec, &self.layer_gpu.pipelines.corners, src, bounds, dst, slot, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    /// The spill's parameters at `width` × `height`, or `None` when it
    /// lays nothing.
    fn spill_params(spill: &crate::layer::Spill, width: u32, height: u32) -> Option<(Params, f32)> {
        let mut params = Params::new(width, height);
        params.opacity = spill.opacity.clamp(0.0, 1.0);
        params.offset_x = (spill.offset_x * width as f32).round() as i32;
        params.offset_y = (spill.offset_y * width as f32).round() as i32;
        let sigma = spill.blur.max(0.0) * width as f32;
        if params.opacity <= 0.001 || (params.offset_x == 0 && params.offset_y == 0 && sigma < 0.5) {
            return None;
        }
        params.color = [
            crate::cpu::decode(spill.color[0] as f32 / 255.0),
            crate::cpu::decode(spill.color[1] as f32 / 255.0),
            crate::cpu::decode(spill.color[2] as f32 / 255.0),
            0.0,
        ];
        Some((params, sigma))
    }

    /// Slots `record_spills` claims for `run`: each spill's own and
    /// its blur's, and the final layer's.
    pub(super) fn spill_run_slots(run: &[crate::layer::Spill], width: u32, height: u32) -> usize {
        let blurs = |sigma: f32| if sigma >= 0.5 { crate::blur::boxes_for_gaussian(sigma).iter().filter(|r| **r != 0).count() } else { 0 };
        1 + run.iter().filter_map(|s| Self::spill_params(s, width, height)).map(|(_, sigma)| 1 + blurs(sigma)).sum::<usize>()
    }

    /// [`Self::layer_spills`], recorded: whether anything was laid (if
    /// not, nothing was recorded and the picture is `src`). The caller's
    /// `cast` and `dst` are the blur's two buffers until the last pass
    /// writes `dst`.
    ///
    /// A cast's colour is the same everywhere; only its alpha varies. So
    /// the alpha alone is cast and blurred, one float a pixel, and then
    /// laid in the colour (#2102). And only where it can show (#2107):
    /// the layer's bounds, found on the GPU, moved by the casts' offsets
    /// and grown by their blurs' reach. A layer placed small in the frame
    /// had its casts worked over the whole frame. Outside that region
    /// every cast is nothing and the layer transparent, so the last pass
    /// lays what the whole-frame passes laid there. The sums run in the
    /// same order as before, the region's outside read as the nothing it
    /// is, so the alpha is the same.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_spills(
        &self,
        rec: &mut EffectRecording<'_>,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        cast: &wgpu::Buffer,
        width: u32,
        height: u32,
        run: &[crate::layer::Spill],
    ) -> bool {
        // Laid nearest-last, as the CPU lays them.
        let laid: Vec<(Params, Vec<usize>)> = run
            .iter()
            .rev()
            .filter_map(|s| Self::spill_params(s, width, height))
            .map(|(params, sigma)| {
                let boxes = if sigma >= 0.5 { crate::blur::boxes_for_gaussian(sigma).into_iter().filter(|r| *r != 0).collect() } else { Vec::new() };
                (params, boxes)
            })
            .collect();
        let Some((bottom, _)) = laid.first() else { return false };
        let q = &self.queue;
        let whole = (width * height).div_ceil(WORKGROUP_SIZE);
        let base = SpillParams { width, height, radius: 0, mode: 0, offset_x: 0, offset_y: 0, opacity: 0.0, pad: 0, reach: [0; 4], color: [0.0; 4] };
        let mut reach = [0i32; 4];
        for (params, boxes) in &laid {
            let r = boxes.iter().sum::<usize>() as i32;
            reach[0] = reach[0].max(r - params.offset_x);
            reach[1] = reach[1].max(r + params.offset_x);
            reach[2] = reach[2].max(r - params.offset_y);
            reach[3] = reach[3].max(r + params.offset_y);
        }
        let run_slot = rec.slots.claim(q, &SpillParams { reach, color: bottom.color, ..base }).expect("slots checked");
        // The region: the layer's bounds, then what the casts reach.
        self.record_spill_pass(rec, RESET, src, src, cast, run_slot, None);
        self.record_spill_pass(rec, BOUNDS_PASS, src, src, cast, run_slot, Some(whole));
        self.record_spill_pass(rec, REGION, src, src, cast, run_slot, Some(1));
        let mut under = &self.layer_gpu.scratch[0];
        let mut next = &self.layer_gpu.scratch[1];
        for (i, (params, boxes)) in laid.iter().enumerate() {
            let own = SpillParams { mode: u32::from(i > 0), offset_x: params.offset_x, offset_y: params.offset_y, opacity: params.opacity, color: params.color, ..base };
            let slot = rec.slots.claim(q, &own).expect("slots checked");
            self.record_spill_pass(rec, CAST_PASS, src, src, cast, slot, None);
            for &radius in boxes {
                let blur = rec.slots.claim(q, &SpillParams { radius: radius as i32, ..base }).expect("slots checked");
                self.record_spill_pass(rec, BLUR_H, cast, src, dst, blur, None);
                self.record_spill_pass(rec, BLUR_V, dst, src, cast, blur, None);
            }
            // The first cast is laid alone; nothing is under it yet.
            if i == 0 {
                self.record_spill_pass(rec, LAY, cast, src, under, slot, None);
            } else {
                self.record_spill_pass(rec, LAY, cast, under, next, slot, None);
                std::mem::swap(&mut under, &mut next);
            }
        }
        self.record_spill_pass(rec, FINAL, src, under, dst, run_slot, Some(whole));
        true
    }

    /// One of the spills' passes: over `workgroups`, or over the region
    /// [`SPILL_REGION`] sized.
    #[allow(clippy::too_many_arguments)]
    fn record_spill_pass(
        &self,
        rec: &mut EffectRecording<'_>,
        pass: usize,
        input: &wgpu::Buffer,
        second: &wgpu::Buffer,
        output: &wgpu::Buffer,
        slot: usize,
        workgroups: Option<u32>,
    ) {
        let lg = &self.layer_gpu;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("spill_pass"),
            layout: &lg.pipelines.spill_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: second.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: rec.slots.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: lg.spill_region.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: if pass == REGION { &lg.spill_args } else { &lg.spill_unused }.as_entire_binding() },
            ],
        });
        let mut cpass = rec.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(SPILL[pass].0), timestamp_writes: None });
        cpass.set_pipeline(&lg.pipelines.spill[pass]);
        cpass.set_bind_group(0, &bind_group, &[]);
        match workgroups {
            Some(n) => crate::gpu::dispatch_flat(&mut cpass, n),
            // The reset is one thread; the region's passes are sized by it.
            None if pass == RESET => crate::gpu::dispatch_flat(&mut cpass, 1),
            // A thread a row, a thread a column, else a thread a pixel.
            None if pass == BLUR_H => cpass.dispatch_workgroups_indirect(&lg.spill_args, 12),
            None if pass == BLUR_V => cpass.dispatch_workgroups_indirect(&lg.spill_args, 24),
            None => cpass.dispatch_workgroups_indirect(&lg.spill_args, 0),
        }
    }
}

pub(super) const ROWS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>; // not-pixels: per-row opaque bounds
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn rows(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let y = gid.x;
    if y >= params.height { return; }
    var left = params.width;
    var right = 0u;
    for (var x = 0u; x < params.width; x++) {
        if unpack(input[y * params.width + x]).a > 0.001 {
            left = min(left, x);
            right = x + 1u;
        }
    }
    output[y] = vec4<f32>(f32(left), f32(y), f32(right), f32(y + 1u));
}
"#
);

pub(super) const BOUNDS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>; // not-pixels: per-row opaque bounds
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn bounds(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    if gid.x != 0u { return; }
    var box = vec4<f32>(f32(params.width), f32(params.height), 0.0, 0.0);
    for (var y = 0u; y < params.height; y++) {
        let row = input[y];
        if row.z > row.x { box = vec4<f32>(min(box.xy, row.xy), max(box.zw, row.zw)); }
    }
    output[0] = box;
}
"#
);

pub(super) const CAST: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn make_cast(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width) - params.offset_x;
    let y = i32(idx / params.width) - params.offset_y;
    var a = 0.0;
    if x >= 0 && y >= 0 && x < i32(params.width) && y < i32(params.height) {
        a = unpack(input[u32(y) * params.width + u32(x)]).a * params.opacity;
    }
    output[idx] = pack(vec4<f32>(params.color.rgb, a));
}
"#
);

/// What every spill pass declares: its parameters ([`SpillParams`]) and
/// the region (`x0, y0, x1, y1`).
macro_rules! spill_head {
    () => {
        r#"
struct Params {
    width: u32, height: u32, radius: i32, mode: u32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    reach: vec4<i32>,
    color: vec4<f32>,
}
struct Region { x0: atomic<i32>, y0: atomic<i32>, x1: atomic<i32>, y1: atomic<i32> }
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var<storage, read_write> region: Region; // not-pixels: the region spills work in
fn rect() -> vec4<i32> {
    return vec4<i32>(atomicLoad(&region.x0), atomicLoad(&region.y0), atomicLoad(&region.x1), atomicLoad(&region.y1));
}
// The region `r`'s `i`th pixel, or (-1, -1) past its end. The region is
// read once a pass's invocation, into `r`: atomics are not cached.
fn region_pixel(r: vec4<i32>, i: u32) -> vec2<i32> {
    let w = r.z - r.x;
    if w <= 0 || i32(i) >= w * (r.w - r.y) { return vec2<i32>(-1, -1); }
    return vec2<i32>(r.x + i32(i) % w, r.y + i32(i) / w);
}
fn inside(r: vec4<i32>, x: i32, y: i32) -> bool {
    return x >= r.x && x < r.z && y >= r.y && y < r.w;
}
// The fixed-point step of a box blur's window: as fine as a window of
// `2 * radius + 1` alphas of at most 1 can hold in a u32 without overflow,
// with room for the float step's own rounding (2^24 to spare).
fn fixed_scale() -> f32 {
    return f32(0xFF000000u / u32(2 * params.radius + 1));
}
"#
    };
}

/// Empty bounds, before [`SPILL_BOUNDS`] grows them.
const SPILL_RESET: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@compute @workgroup_size(1)
fn spill_reset() {
    atomicStore(&region.x0, i32(params.width));
    atomicStore(&region.y0, i32(params.height));
    atomicStore(&region.x1, 0);
    atomicStore(&region.y1, 0);
}
"#);

/// The bounds of the layer's pixels that are not wholly transparent.
const SPILL_BOUNDS: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
var<workgroup> wx0: atomic<i32>;
var<workgroup> wy0: atomic<i32>;
var<workgroup> wx1: atomic<i32>;
var<workgroup> wy1: atomic<i32>;
@compute @workgroup_size(256)
fn spill_bounds(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    if li == 0u {
        atomicStore(&wx0, i32(params.width));
        atomicStore(&wy0, i32(params.height));
        atomicStore(&wx1, 0);
        atomicStore(&wy1, 0);
    }
    workgroupBarrier();
    let idx = gid.x;
    if idx < params.width * params.height && unpack(input[idx]).a > 0.0 {
        let x = i32(idx % params.width);
        let y = i32(idx / params.width);
        atomicMin(&wx0, x);
        atomicMin(&wy0, y);
        atomicMax(&wx1, x + 1);
        atomicMax(&wy1, y + 1);
    }
    workgroupBarrier();
    if li == 0u && atomicLoad(&wx1) > atomicLoad(&wx0) {
        atomicMin(&region.x0, atomicLoad(&wx0));
        atomicMin(&region.y0, atomicLoad(&wy0));
        atomicMax(&region.x1, atomicLoad(&wx1));
        atomicMax(&region.y1, atomicLoad(&wy1));
    }
}
"#);

/// The bounds grown by the casts' reach and kept in the frame, and the
/// dispatch that covers them: nothing when the layer is all transparent.
const SPILL_REGION: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@group(0) @binding(5) var<storage, read_write> args: array<u32, 12>; // not-pixels: the region's indirect dispatches
// `n` workgroups over x and, past the per-dimension limit, y, as
// `split_workgroups` splits a direct dispatch (#2341): the passes rebuild
// their index from both.
fn split(at: u32, n: u32) {
    var x = n;
    var y = 1u;
    if n > 65535u {
        y = (n + 65534u) / 65535u;
        x = (n + y - 1u) / y;
    }
    args[at] = x;
    args[at + 1u] = y;
    args[at + 2u] = 1u;
}
@compute @workgroup_size(1)
fn spill_region() {
    let b = rect();
    var r = vec4<i32>(0);
    if b.z > b.x && b.w > b.y {
        r = vec4<i32>(
            max(b.x - max(params.reach.x, 0), 0),
            max(b.y - max(params.reach.z, 0), 0),
            min(b.z + max(params.reach.y, 0), i32(params.width)),
            min(b.w + max(params.reach.w, 0), i32(params.height)),
        );
    }
    atomicStore(&region.x0, r.x);
    atomicStore(&region.y0, r.y);
    atomicStore(&region.x1, r.z);
    atomicStore(&region.y1, r.w);
    let n = u32((r.z - r.x) * (r.w - r.y));
    // A thread a pixel; a thread a row; a thread a column.
    split(0u, (n + 255u) / 256u);
    // A thread a 32-pixel stretch of a row, or of a column.
    let rw = u32(r.z - r.x);
    let rh = u32(r.w - r.y);
    split(3u, (rh * ((rw + 31u) / 32u) + 63u) / 64u);
    split(6u, (rw * ((rh + 31u) / 32u) + 63u) / 64u);
}
"#);

/// [`CAST`]'s alpha alone over the region, one float a pixel at its
/// place in the frame: the colour is laid by [`SPILL_LAY`].
const SPILL_CAST: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<f32>; // not-pixels: a cast's alpha
@compute @workgroup_size(256)
fn spill_cast(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let r = rect();
    let p = region_pixel(r, gid.x);
    if p.x < 0 { return; }
    let x = p.x - params.offset_x;
    let y = p.y - params.offset_y;
    var a = 0.0;
    if x >= 0 && y >= 0 && x < i32(params.width) && y < i32(params.height) {
        a = unpack(input[u32(y) * params.width + u32(x)]).a * params.opacity;
    }
    output[u32(p.y) * params.width + u32(p.x)] = a;
}
"#);

/// The box blur's horizontal pass over a cast's alpha in the region, a
/// thread a 32-pixel stretch of a row: the window clamped at the frame's
/// edge, the region's outside read as nothing, summed once at the
/// stretch's start and slid along it, the pixel coming in
/// added and the one going out taken away (#2107). Summed afresh at each
/// pixel it read the window's every pixel, 140 at a 4K glow's reach, and
/// was most of a card's cost. Slid in floats, what came in and went out
/// left a residue where the window holds nothing, and a cast's nothing
/// was not nothing; so the window is summed in fixed point, exactly, each
/// alpha (0..1, as a cast's is) in the finest steps a window of its size
/// can hold, and nothing stays nothing.
const SPILL_BLUR_H: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@group(0) @binding(0) var<storage, read> input: array<f32>; // not-pixels: a cast's alpha
@group(0) @binding(2) var<storage, read_write> output: array<f32>; // not-pixels: a cast's alpha
fn at_x(r: vec4<i32>, x: i32, y: i32) -> u32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    if cx < r.x || cx >= r.z { return 0u; }
    return u32(round(clamp(input[u32(y) * params.width + u32(cx)], 0.0, 1.0) * fixed_scale()));
}
@compute @workgroup_size(64)
fn spill_blur_h(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 64u, 0u, gid_flat.z);
    let r = rect();
    let stretches = (r.z - r.x + 31) / 32;
    if stretches <= 0 { return; }
    let y = r.y + i32(gid.x) / stretches;
    if y >= r.w { return; }
    let x0 = r.x + (i32(gid.x) % stretches) * 32;
    let x1 = min(x0 + 32, r.z);
    let inv = 1.0 / (f32(2 * params.radius + 1) * fixed_scale());
    var sum = 0u;
    for (var dx = -params.radius; dx <= params.radius; dx++) {
        sum += at_x(r, x0 + dx, y);
    }
    for (var x = x0; x < x1; x++) {
        output[u32(y) * params.width + u32(x)] = f32(sum) * inv;
        sum = sum + at_x(r, x + params.radius + 1, y) - at_x(r, x - params.radius, y);
    }
}
"#);

/// [`SPILL_BLUR_H`]'s vertical pass, a thread a 32-pixel stretch of a
/// column, top to bottom.
const SPILL_BLUR_V: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@group(0) @binding(0) var<storage, read> input: array<f32>; // not-pixels: a cast's alpha
@group(0) @binding(2) var<storage, read_write> output: array<f32>; // not-pixels: a cast's alpha
fn at_y(r: vec4<i32>, x: i32, y: i32) -> u32 {
    let cy = clamp(y, 0, i32(params.height) - 1);
    if cy < r.y || cy >= r.w { return 0u; }
    return u32(round(clamp(input[u32(cy) * params.width + u32(x)], 0.0, 1.0) * fixed_scale()));
}
@compute @workgroup_size(64)
fn spill_blur_v(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 64u, 0u, gid_flat.z);
    let r = rect();
    let cols = r.z - r.x;
    if cols <= 0 { return; }
    // Neighbouring threads take neighbouring columns, so they read
    // neighbouring pixels.
    let x = r.x + i32(gid.x) % cols;
    let y0 = r.y + (i32(gid.x) / cols) * 32;
    if y0 >= r.w { return; }
    let y1 = min(y0 + 32, r.w);
    let inv = 1.0 / (f32(2 * params.radius + 1) * fixed_scale());
    var sum = 0u;
    for (var dy = -params.radius; dy <= params.radius; dy++) {
        sum += at_y(r, x, y0 + dy);
    }
    for (var y = y0; y < y1; y++) {
        output[u32(y) * params.width + u32(x)] = f32(sum) * inv;
        sum = sum + at_y(r, x, y + params.radius + 1) - at_y(r, x, y - params.radius);
    }
}
"#);

/// A cast laid in its colour over the region: alone (`mode` 0), or over
/// the casts laid before it, by [`OVER`]'s rule.
const SPILL_LAY: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@group(0) @binding(0) var<storage, read> input: array<f32>; // not-pixels: a cast's alpha
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(256)
fn spill_lay(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let r = rect();
    let p = region_pixel(r, gid.x);
    if p.x < 0 { return; }
    let idx = u32(p.y) * params.width + u32(p.x);
    let top = vec4<f32>(params.color.rgb, input[idx]);
    if params.mode == 0u {
        output[idx] = pack(top);
        return;
    }
    let bottom = unpack(second[idx]);
    let a = top.a + bottom.a * (1.0 - top.a);
    var pixel = bottom;
    if a > 0.0 {
        pixel = vec4<f32>((top.rgb * top.a + bottom.rgb * bottom.a * (1.0 - top.a)) / a, a);
    }
    output[idx] = pack(pixel);
}
"#);

/// The layer over its casts, over the whole frame by [`OVER`]'s rule:
/// outside the region the casts are nothing, the first laid's colour at
/// no alpha, as the whole-frame passes left them.
const SPILL_FINAL: &str = concat!(shader_pixel_storage_wgsl!(), spill_head!(), r#"
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(256)
fn spill_final(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let top = unpack(input[idx]);
    var bottom = vec4<f32>(params.color.rgb, 0.0);
    if inside(rect(), i32(idx % params.width), i32(idx / params.width)) {
        bottom = unpack(second[idx]);
    }
    let a = top.a + bottom.a * (1.0 - top.a);
    var pixel = bottom;
    if a > 0.0 {
        pixel = vec4<f32>((top.rgb * top.a + bottom.rgb * bottom.a * (1.0 - top.a)) / a, a);
    }
    output[idx] = pack(pixel);
}
"#);

/// The spills' passes: name, source, entry point.
pub(super) const SPILL: [(&str, &str, &str); SPILL_PASSES] = [
    ("LAYER_SPILL_RESET", SPILL_RESET, "spill_reset"),
    ("LAYER_SPILL_BOUNDS", SPILL_BOUNDS, "spill_bounds"),
    ("LAYER_SPILL_REGION", SPILL_REGION, "spill_region"),
    ("LAYER_SPILL_CAST", SPILL_CAST, "spill_cast"),
    ("LAYER_SPILL_BLUR_H", SPILL_BLUR_H, "spill_blur_h"),
    ("LAYER_SPILL_BLUR_V", SPILL_BLUR_V, "spill_blur_v"),
    ("LAYER_SPILL_LAY", SPILL_LAY, "spill_lay"),
    ("LAYER_SPILL_FINAL", SPILL_FINAL, "spill_final"),
];

pub(super) const CORNERS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@compute @workgroup_size(256)
fn corners(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let box = second[0];
    let short = min(box.z - box.x, box.w - box.y);
    let r = params.radius * short;
    var pixel = unpack(input[idx]);
    if box.z > box.x && r > 0.5 {
        let pos = vec2<f32>(f32(idx % params.width), f32(idx / params.width)) + vec2<f32>(0.5);
        let dxy = max(max(box.xy + vec2<f32>(r) - pos, pos - (box.zw - vec2<f32>(r))), vec2<f32>(0.0));
        let d = length(dxy) - r;
        pixel.a *= clamp(0.5 - d / max(params.feather * short, 1.0), 0.0, 1.0);
    }
    output[idx] = pack(pixel);
}
"#
);

pub(super) const OVER: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@compute @workgroup_size(256)
fn over(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let top = unpack(input[idx]);
    let bottom = unpack(second[idx]);
    let a = top.a + bottom.a * (1.0 - top.a);
    var pixel = bottom;
    if a > 0.0 {
        pixel = vec4<f32>((top.rgb * top.a + bottom.rgb * bottom.a * (1.0 - top.a)) / a, a);
    }
    output[idx] = pack(pixel);
}
"#
);

/// Linear between the four pixels round `(x, y)`, clamped to `lo..=hi`:
/// `crate::layer::bilinear`, for a buffer bound as `buf`.
macro_rules! fill_bilinear_wgsl {
    ($buf:literal) => {
        concat!(
            "fn bilinear_", $buf, "(x_in: f32, y_in: f32, w: u32, lo: vec2<f32>, hi: vec2<f32>) -> vec4<f32> {\n",
            "    let x = clamp(x_in, lo.x, hi.x);\n",
            "    let y = clamp(y_in, lo.y, hi.y);\n",
            "    let ix = floor(x);\n",
            "    let iy = floor(y);\n",
            "    let tx = x - ix;\n",
            "    let ty = y - iy;\n",
            "    let x0 = u32(ix); let y0 = u32(iy);\n",
            "    let x1 = u32(min(ix + 1.0, hi.x)); let y1 = u32(min(iy + 1.0, hi.y));\n",
            "    let a = unpack(", $buf, "[y0 * w + x0]); let b = unpack(", $buf, "[y0 * w + x1]);\n",
            "    let c = unpack(", $buf, "[y1 * w + x0]); let d = unpack(", $buf, "[y1 * w + x1]);\n",
            "    let top = a + (b - a) * tx;\n",
            "    let bottom = c + (d - c) * tx;\n",
            "    return top + (bottom - top) * ty;\n",
            "}\n",
        )
    };
}

pub(super) const FILL_SMALL: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct FillParams {
    width: u32, height: u32, small_w: u32, small_h: u32,
    zoom: f32, darken: f32, pad0: u32, pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: FillParams;
"#,
    fill_bilinear_wgsl!("input"),
    r#"
@compute @workgroup_size(256)
fn fill_small(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.small_w * params.small_h { return; }
    let box = second[0];
    let wf = f32(params.width);
    let hf = f32(params.height);
    // Nothing opaque, or a picture that fills the frame: no bars, and an
    // empty fill says so to fill_over.
    if !(box.z > box.x) || (box.x <= 0.0 && box.y <= 0.0 && box.z >= wf && box.w >= hf) {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let scale = max(wf / (box.z - box.x), hf / (box.w - box.y)) * max(params.zoom, 1.0);
    let cx = (box.x + box.z) * 0.5;
    let cy = (box.y + box.w) * 0.5;
    let fx = (f32(idx % params.small_w) + 0.5) * wf / f32(params.small_w);
    let fy = (f32(idx / params.small_w) + 0.5) * hf / f32(params.small_h);
    let c = bilinear_input(cx + (fx - wf * 0.5) / scale - 0.5, cy + (fy - hf * 0.5) / scale - 0.5, params.width, box.xy, box.zw - vec2<f32>(1.0));
    output[idx] = pack(vec4<f32>(c.rgb, 1.0));
}
"#
);

pub(super) const FILL_OVER: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct FillParams {
    width: u32, height: u32, small_w: u32, small_h: u32,
    zoom: f32, darken: f32, pad0: u32, pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: FillParams;
"#,
    fill_bilinear_wgsl!("second"),
    r#"
@compute @workgroup_size(256)
fn fill_over(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let rx = f32(params.small_w) / f32(params.width);
    let ry = f32(params.small_h) / f32(params.height);
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let hi = vec2<f32>(f32(params.small_w - 1u), f32(params.small_h - 1u));
    let bg = bilinear_second((x + 0.5) * rx - 0.5, (y + 0.5) * ry - 0.5, params.small_w, vec2<f32>(0.0), hi);
    if bg.a < 0.5 {
        output[idx] = input[idx];
        return;
    }
    let px = unpack(input[idx]);
    let keep = 1.0 - params.darken;
    output[idx] = pack(vec4<f32>(px.rgb * px.a + bg.rgb * keep * (1.0 - px.a), 1.0));
}
"#
);
