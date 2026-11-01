//! A kept frame's ten-bit words to display bytes on the GPU (#3300).
//!
//! A frame drawn ahead (#3212) is kept as ten-bit words and turned into
//! display bytes as it is read for the screen. On the CPU that is a table
//! look-up, a dither and a rounding per channel: 6 ms a 4K frame on a fast
//! desktop and about 20 ms on an Apple M1, most of what loading a kept
//! frame costs there. Here the words go up (4 bytes a pixel, as the bytes
//! come back) and one dispatch makes the bytes, through the same table of
//! the 1024 codes, made on the CPU: the bytes are the CPU's, bit for bit.

use crate::gpu_context::GpuContext;

const SHADER: &str = gpu_shader!("output/ten_bit_display.wgsl");

/// The shader, as compiled, for the shader checks.
pub(super) const SHADER_TEN_BIT_DISPLAY: &str = SHADER;

/// Each of the 1024 codes' display value, as [`crate::dither::quantise_parts`]
/// gives it at 8 bits: times 255 and clamped, and 1 where it is dithered.
pub fn codes(display_of_code: impl Fn(u32) -> f32) -> Vec<[f32; 2]> {
    (0..1024)
        .map(|c| {
            let (x, dithered) = crate::dither::quantise_parts(display_of_code(c), 255.0);
            [x, if dithered { 1.0 } else { 0.0 }]
        })
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    _pad: [u32; 2],
}

/// The pipeline and buffers, kept between frames; the buffers grow to the
/// largest frame asked for.
pub struct TenBitDisplay {
    ctx: GpuContext,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    codes: wgpu::Buffer,
    /// Words in, bytes out, and the mappable copy: `bytes` each.
    frame: Option<(u64, wgpu::Buffer, wgpu::Buffer, wgpu::Buffer)>,
}

impl TenBitDisplay {
    /// On `ctx`'s device. `None` when the device is lost.
    pub fn new(ctx: &GpuContext) -> Option<Self> {
        if ctx.device_lost_flag().load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let device = &ctx.device;
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ten_bit_display"),
            entries: &[
                storage(0, true),
                storage(1, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(3, true),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("ten_bit_display"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("ten_bit_display"), bind_group_layouts: &[&layout], push_constant_ranges: &[] });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ten_bit_display"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ten_bit_display_params"), size: std::mem::size_of::<Params>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let codes = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ten_bit_display_codes"), size: 1024 * 8, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Some(Self { ctx: ctx.clone(), pipeline, layout, params, codes, frame: None })
    }

    /// `words` (`width` × `height` of them, little-endian) as display
    /// bytes through `codes` ([`codes`]). `None` when the GPU can't (the
    /// device lost, a frame past its limits, the readback failing): the
    /// CPU's encode then.
    pub fn encode(&mut self, words: &[u8], width: u32, height: u32, codes: &[[f32; 2]]) -> Option<Vec<u8>> {
        let bytes = (width as u64) * (height as u64) * 4;
        if bytes == 0 || words.len() as u64 != bytes || codes.len() != 1024 || self.ctx.device_lost_flag().load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let device = &self.ctx.device;
        if bytes > device.limits().max_storage_buffer_binding_size as u64 {
            return None;
        }
        if self.frame.as_ref().is_none_or(|f| f.0 < bytes) {
            let make = |label, usage| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes, usage, mapped_at_creation: false });
            self.frame = Some((
                bytes,
                make("ten_bit_display_words", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST),
                make("ten_bit_display_bytes", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC),
                make("ten_bit_display_staging", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
            ));
        }
        let (_, input, output, staging) = self.frame.as_ref()?;
        let queue = &self.ctx.queue;
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&Params { width, height, _pad: [0; 2] }));
        queue.write_buffer(&self.codes, 0, bytemuck::cast_slice(codes));
        queue.write_buffer(input, 0, words);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ten_bit_display"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.codes.as_entire_binding() },
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ten_bit_display") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ten_bit_display"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            super::dispatch_flat(&mut pass, ((width as u64 * height as u64).div_ceil(256)) as u32);
        }
        enc.copy_buffer_to_buffer(output, 0, staging, 0, bytes);
        queue.submit(std::iter::once(enc.finish()));
        let slice = staging.slice(..bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let read = match super::poll_until_mapped(device, &rx) {
            Ok(Ok(())) => Some(slice.get_mapped_range().to_vec()),
            _ => None,
        };
        staging.unmap();
        read
    }
}
