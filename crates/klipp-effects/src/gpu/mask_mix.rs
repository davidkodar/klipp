//! Record-only mask mixing with explicit image resources.
use super::{GpuEffectsPipeline, WORKGROUP_SIZE};
use crate::mask_mix::{MaskMix, MaskMixError};

pub(super) struct MaskMixGpu {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    /// The mix with a window's shapes drawn inside it (#1252): input,
    /// effect, shape records, output, params, bezier points.
    window_layout: wgpu::BindGroupLayout,
    window_pipeline: wgpu::ComputePipeline,
}

/// A compute pipeline and its layout: a storage buffer at every binding
/// but `uniform`, read-only but at `read_write`.
fn make_pipeline(device: &wgpu::Device, label: &str, source: &str, bindings: u32, uniform: u32, read_write: u32) -> (wgpu::BindGroupLayout, wgpu::ComputePipeline) {
    let entries: Vec<_> = (0..bindings)
        .map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: if binding == uniform {
                    wgpu::BufferBindingType::Uniform
                } else {
                    wgpu::BufferBindingType::Storage { read_only: binding != read_write }
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        })
        .collect();
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries: &entries });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[&layout], push_constant_ranges: &[] });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pl),
        module: &module,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    });
    (layout, pipeline)
}

impl MaskMixGpu {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let (layout, pipeline) = make_pipeline(device, "mask_mix", SHADER_MASK_MIX, 5, 4, 3);
        let (window_layout, window_pipeline) = make_pipeline(device, "mask_mix_window", SHADER_MASK_MIX_WINDOW, 6, 4, 3);
        Self { layout, pipeline, window_layout, window_pipeline }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    mix: f32,
    channel: u32,
    has_mask: u32,
    alpha_one: u32,
    /// The window's shape count, for the window mix (#1252); 0 otherwise.
    mask_count: u32,
    pad: u32,
    luma: [f32; 4],
}

impl GpuEffectsPipeline {
    /// Record a mix into the caller's encoder; never submit or read back.
    ///
    /// Buffers contain straight linear RGBA and must share the pixel grid.
    /// Output must not alias an input. An absent mask is white. The caller
    /// owns resource lifetimes and submits on this pipeline's queue.
    /// Each call reserves one chain params slot; `ParamsExhausted` records
    /// nothing. Submit all recorded work before `begin_chain` resets the
    /// ring, and never reuse it across outstanding unsubmitted encoders.
    #[allow(clippy::too_many_arguments)]
    pub fn record_mask_mix(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::Buffer,
        effect: &wgpu::Buffer,
        mask: Option<&wgpu::Buffer>,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        settings: MaskMix,
        legacy_alpha_one: bool,
    ) -> Result<(), MaskMixError> {
        let Some(slot) = self.claim_mask_mix(width, height, settings, mask.is_some(), legacy_alpha_one)? else {
            return Ok(());
        };
        self.record_mask_mix_in(encoder, input, effect, mask, output, width, height, slot)
    }

    /// The uniform slot a mix at `width` x `height` reads, its parameters
    /// written; `None` for an empty frame, which records nothing.
    pub(crate) fn claim_mask_mix(&mut self, width: u32, height: u32, settings: MaskMix, has_mask: bool, legacy_alpha_one: bool) -> Result<Option<usize>, MaskMixError> {
        self.claim_mask_mix_with(width, height, settings, has_mask, legacy_alpha_one, 0)
    }

    /// [`Self::claim_mask_mix`] for the window mix: its mask is the window's
    /// `mask_count` shapes ([`Self::record_mask_mix_window_in`]).
    pub(crate) fn claim_mask_mix_window(&mut self, width: u32, height: u32, settings: MaskMix, mask_count: u32) -> Result<Option<usize>, MaskMixError> {
        self.claim_mask_mix_with(width, height, settings, true, false, mask_count)
    }

    fn claim_mask_mix_with(&mut self, width: u32, height: u32, settings: MaskMix, has_mask: bool, legacy_alpha_one: bool, mask_count: u32) -> Result<Option<usize>, MaskMixError> {
        let pixels = width.checked_mul(height).ok_or(MaskMixError::SizeMismatch)?;
        if pixels == 0 {
            return Ok(None);
        }
        if self.chain_params.remaining() == 0 {
            return Err(MaskMixError::ParamsExhausted);
        }
        let slot = self.next_params_slot();
        let params = Params {
            width,
            height,
            mix: settings.mix,
            channel: settings.channel as u32,
            has_mask: u32::from(has_mask),
            alpha_one: u32::from(legacy_alpha_one),
            mask_count,
            pad: 0,
            luma: [settings.luma[0], settings.luma[1], settings.luma[2], 0.0],
        };
        self.write_chain_params(slot, &params);
        Ok(Some(slot))
    }

    /// Record the mix whose parameters `slot` holds ([`Self::claim_mask_mix`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_mask_mix_in(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::Buffer,
        effect: &wgpu::Buffer,
        mask: Option<&wgpu::Buffer>,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        slot: usize,
    ) -> Result<(), MaskMixError> {
        let pixels = width.checked_mul(height).ok_or(MaskMixError::SizeMismatch)?;
        let bytes = u64::from(pixels) * 16;
        if [input, effect, output].iter().any(|b| b.size() < bytes) || mask.is_some_and(|b| b.size() < bytes) {
            return Err(MaskMixError::SizeMismatch);
        }
        let buffers = [input, effect, mask.unwrap_or(input), output, self.chain_params.buffer(slot)];
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(binding, b)| wgpu::BindGroupEntry { binding: binding as u32, resource: b.as_entire_binding() })
            .collect();
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask_mix"),
            layout: &self.mask_mix_gpu.layout,
            entries: &entries,
        });
        Self::record_compute_dispatch(encoder, &self.mask_mix_gpu.pipeline, &group, pixels.div_ceil(WORKGROUP_SIZE));
        Ok(())
    }

    /// Record the window mix whose parameters `slot` holds
    /// ([`Self::claim_mask_mix_window`]): the window's coverage is worked out
    /// per pixel inside the lerp, from its shape `records` and bezier
    /// `points`, with no matte written or read (#1252). The same coverage as
    /// the window drawn into a matte and mixed through it, up to float
    /// rounding (`tests/window_mix.rs`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_mask_mix_window_in(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::Buffer,
        effect: &wgpu::Buffer,
        records: &wgpu::Buffer,
        points: &wgpu::Buffer,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        slot: usize,
    ) -> Result<(), MaskMixError> {
        let pixels = width.checked_mul(height).ok_or(MaskMixError::SizeMismatch)?;
        let bytes = u64::from(pixels) * 16;
        if [input, effect, output].iter().any(|b| b.size() < bytes) {
            return Err(MaskMixError::SizeMismatch);
        }
        let buffers = [input, effect, records, output, self.chain_params.buffer(slot), points];
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(binding, b)| wgpu::BindGroupEntry { binding: binding as u32, resource: b.as_entire_binding() })
            .collect();
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask_mix_window"),
            layout: &self.mask_mix_gpu.window_layout,
            entries: &entries,
        });
        Self::record_compute_dispatch(encoder, &self.mask_mix_gpu.window_pipeline, &group, pixels.div_ceil(WORKGROUP_SIZE));
        Ok(())
    }
}

pub(super) const SHADER_MASK_MIX: &str = concat!(
    shader_linear_helpers_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, mix: f32, channel: u32,
    has_mask: u32, alpha_one: u32, mask_count: u32, pad1: u32,
    luma: vec4<f32>,
};
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> effect: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> mask: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(4) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    if idx >= params.width * params.height { return; }
    var coverage = 1.0;
    if params.has_mask != 0u {
        let m = unpack(mask[idx]);
        coverage = m.r;
        if params.channel == 1u { coverage = dot(m.rgb, params.luma.rgb); }
        if params.channel == 2u { coverage = m.a; }
        if (bitcast<u32>(coverage) & 0x7f800000u) == 0x7f800000u { coverage = 0.0; }
        coverage = clamp(coverage, 0.0, 1.0);
    }
    let w = clamp(params.mix, 0.0, 1.0) * coverage;
    // Copy stored pixels at exact endpoints, preserving their bit patterns.
    if params.alpha_one == 0u && w == 0.0 {
        output[idx] = input[idx];
        return;
    }
    if params.alpha_one == 0u && w == 1.0 {
        output[idx] = effect[idx];
        return;
    }
    var value = unpack(input[idx]);
    if w == 1.0 { value = unpack(effect[idx]); }
    else if w != 0.0 { value = value + (unpack(effect[idx]) - value) * w; }
    if params.alpha_one != 0u { value.a = 1.0; }
    output[idx] = pack(value);
}
"#
);

/// [`SHADER_MASK_MIX`] with the mask a window's shapes, worked out per pixel
/// as `SHADER_MASK_V2` draws them into a matte (`mask_sdf_wgsl`, one copy):
/// the matte `(m, m, m, 1)` that shader would write is never stored, and the
/// coverage read from it is the same arithmetic on the same values.
pub(super) const SHADER_MASK_MIX_WINDOW: &str = concat!(
    shader_linear_helpers_wgsl!(),
    mask_sdf_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, mix: f32, channel: u32,
    has_mask: u32, alpha_one: u32, mask_count: u32, pad1: u32,
    luma: vec4<f32>,
};
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> effect: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> records: array<MaskRecord>;
@group(0) @binding(3) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read> points: array<vec2<f32>>;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    if idx >= params.width * params.height { return; }
    // The window at this pixel, as SHADER_MASK_V2 draws it.
    let x = idx % params.width;
    let y = idx / params.width;
    let nx = f32(x) / f32(max(params.width - 1u, 1u));
    let ny = f32(y) / f32(max(params.height - 1u, 1u));
    let aspect = f32(params.height) / f32(params.width);
    var matte: f32 = 0.0;
    for (var i: u32 = 0u; i < params.mask_count; i = i + 1u) {
        matte = max(matte, eval_record(records[i], nx, ny, aspect));
    }
    let mv = clamp(matte, 0.0, 1.0);
    let m = vec4<f32>(mv, mv, mv, 1.0);
    // From here, SHADER_MASK_MIX with `m` for the matte it would read.
    var coverage = m.r;
    if params.channel == 1u { coverage = dot(m.rgb, params.luma.rgb); }
    if params.channel == 2u { coverage = m.a; }
    if (bitcast<u32>(coverage) & 0x7f800000u) == 0x7f800000u { coverage = 0.0; }
    coverage = clamp(coverage, 0.0, 1.0);
    let w = clamp(params.mix, 0.0, 1.0) * coverage;
    if params.alpha_one == 0u && w == 0.0 {
        output[idx] = input[idx];
        return;
    }
    if params.alpha_one == 0u && w == 1.0 {
        output[idx] = effect[idx];
        return;
    }
    var value = unpack(input[idx]);
    if w == 1.0 { value = unpack(effect[idx]); }
    else if w != 0.0 { value = value + (unpack(effect[idx]) - value) * w; }
    if params.alpha_one != 0u { value.a = 1.0; }
    output[idx] = pack(value);
}
"#
);
