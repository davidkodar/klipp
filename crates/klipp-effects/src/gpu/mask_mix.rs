//! Record-only mask mixing with explicit image resources.
use super::{GpuEffectsPipeline, CHAIN_PARAMS_RING_SIZE, WORKGROUP_SIZE};
use crate::mask_mix::{MaskMix, MaskMixError};

pub(super) struct MaskMixGpu {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}
impl MaskMixGpu {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let entries: Vec<_> = (0..5)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: if binding == 4 {
                        wgpu::BufferBindingType::Uniform
                    } else {
                        wgpu::BufferBindingType::Storage {
                            read_only: binding != 3,
                        }
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask_mix"),
            entries: &entries,
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask_mix"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mask_mix"),
            source: wgpu::ShaderSource::Wgsl(SHADER_MASK_MIX.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mask_mix"),
            layout: Some(&pl),
            module: &module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });
        Self { layout, pipeline }
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
    pad: [u32; 2],
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
        let pixels = width
            .checked_mul(height)
            .ok_or(MaskMixError::SizeMismatch)?;
        if pixels == 0 {
            return Ok(());
        }
        let bytes = u64::from(pixels) * 16;
        if [input, effect, output].iter().any(|b| b.size() < bytes)
            || mask.is_some_and(|b| b.size() < bytes)
        {
            return Err(MaskMixError::SizeMismatch);
        }
        if self.chain_slot_idx as usize >= CHAIN_PARAMS_RING_SIZE {
            return Err(MaskMixError::ParamsExhausted);
        }
        let slot = self.next_params_slot();
        let params = Params {
            width,
            height,
            mix: settings.mix,
            channel: settings.channel as u32,
            has_mask: u32::from(mask.is_some()),
            alpha_one: u32::from(legacy_alpha_one),
            pad: [0; 2],
            luma: [settings.luma[0], settings.luma[1], settings.luma[2], 0.0],
        };
        self.write_chain_params(slot, &params);
        let buffers = [
            input,
            effect,
            mask.unwrap_or(input),
            output,
            &self.params_ring[slot],
        ];
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(binding, b)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask_mix"),
            layout: &self.mask_mix_gpu.layout,
            entries: &entries,
        });
        Self::record_compute_dispatch(
            encoder,
            &self.mask_mix_gpu.pipeline,
            &group,
            pixels.div_ceil(WORKGROUP_SIZE),
        );
        Ok(())
    }
}

pub(super) const SHADER_MASK_MIX: &str = concat!(
    shader_linear_helpers_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, mix: f32, channel: u32,
    has_mask: u32, alpha_one: u32, pad0: u32, pad1: u32,
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
