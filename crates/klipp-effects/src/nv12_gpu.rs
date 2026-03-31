//! GPU-accelerated NV12 → RGBA conversion with optional resize.
//!
//! Uses a standalone wgpu device so it can run on the decoder thread
//! without sharing the main effects pipeline's device/queue.
//! This replaces the CPU-bound swscale path for hardware-decoded frames.

use tracing::{info, warn};

/// NV12 → RGBA GPU converter. Owns its own wgpu device + queue.
pub struct GpuNv12Converter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    // Reusable buffers — recreated when dimensions change
    y_buf: Option<wgpu::Buffer>,
    uv_buf: Option<wgpu::Buffer>,
    output_buf: Option<wgpu::Buffer>,
    staging_buf: Option<wgpu::Buffer>,
    params_buf: wgpu::Buffer,
    // Cached dimensions to avoid re-creating buffers
    cached_y_size: u64,
    cached_uv_size: u64,
    cached_output_size: u64,
}

/// Params uniform for the NV12→RGBA shader (must match WGSL struct layout).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Nv12Params {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    y_stride: u32,
    uv_stride: u32,
    _pad0: u32,
    _pad1: u32,
}

/// The compute shader source — NV12 to RGBA with BT.709 color matrix and nearest-neighbor resize.
/// Uses 2D workgroups (16×16) for spatial locality on the output image.
const SHADER_NV12_TO_RGBA: &str = "
// NV12 to RGBA conversion with optional resize
// NV12: Y plane (full res) + interleaved UV plane (half res)
// BT.709 color matrix

@group(0) @binding(0) var<storage, read> y_plane: array<u32>;
@group(0) @binding(1) var<storage, read> uv_plane: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;

struct Params {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    y_stride: u32,
    uv_stride: u32,
    _pad0: u32,
    _pad1: u32,
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = gid.y;
    if (dx >= params.dst_width || dy >= params.dst_height) { return; }

    // Map output pixel to source pixel (nearest neighbor resize)
    let sx = dx * params.src_width / params.dst_width;
    let sy = dy * params.src_height / params.dst_height;

    // Read Y value (byte access via u32 array)
    let y_byte_idx = sy * params.y_stride + sx;
    let y_word = y_plane[y_byte_idx / 4u];
    let y_shift = (y_byte_idx % 4u) * 8u;
    let y_val = f32((y_word >> y_shift) & 0xFFu);

    // Read UV values (interleaved, half resolution)
    let uv_x = (sx / 2u) * 2u; // U and V are interleaved pairs
    let uv_y = sy / 2u;
    let uv_byte_idx = uv_y * params.uv_stride + uv_x;
    let uv_word = uv_plane[uv_byte_idx / 4u];
    let uv_shift = (uv_byte_idx % 4u) * 8u;
    let u_val = f32((uv_word >> uv_shift) & 0xFFu);
    let uv_byte_idx_v = uv_byte_idx + 1u;
    let uv_word_v = uv_plane[uv_byte_idx_v / 4u];
    let uv_shift_v = (uv_byte_idx_v % 4u) * 8u;
    let v_val = f32((uv_word_v >> uv_shift_v) & 0xFFu);

    // BT.709 YUV to RGB (limited range)
    let y = (y_val - 16.0) / 219.0;
    let u = (u_val - 128.0) / 224.0;
    let v = (v_val - 128.0) / 224.0;

    let r = clamp(y + 1.5748 * v, 0.0, 1.0);
    let g = clamp(y - 0.1873 * u - 0.4681 * v, 0.0, 1.0);
    let b = clamp(y + 1.8556 * u, 0.0, 1.0);

    let ri = u32(r * 255.0);
    let gi = u32(g * 255.0);
    let bi = u32(b * 255.0);
    let pixel = ri | (gi << 8u) | (bi << 16u) | (255u << 24u);

    output[dy * params.dst_width + dx] = pixel;
}
";

impl GpuNv12Converter {
    /// Create a new GPU NV12 converter with its own wgpu device.
    /// Returns `None` if no suitable GPU adapter is found.
    pub fn new() -> Option<Self> {
        // Ensure Vulkan ICD is discoverable
        if std::env::var("VK_DRIVER_FILES").is_err() {
            let icd_path = "/usr/share/vulkan/icd.d/nvidia_icd.json";
            if std::path::Path::new(icd_path).exists() {
                std::env::set_var("VK_DRIVER_FILES", icd_path);
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::from_build_config() & !wgpu::InstanceFlags::VALIDATION,
            ..Default::default()
        });

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))?;

        let adapter_info = adapter.get_info();
        info!(
            "NV12 GPU converter: {} ({:?})",
            adapter_info.name, adapter_info.backend
        );

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("klipp-nv12"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            },
            None,
        ))
        .ok()?;

        // Bind group layout: y_plane (read), uv_plane (read), output (read_write), params (uniform)
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("nv12_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Push error scope to catch shader compilation errors
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12_to_rgba"),
            source: wgpu::ShaderSource::Wgsl(SHADER_NV12_TO_RGBA.into()),
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("nv12_to_rgba"),
            layout: Some(&pipeline_layout),
            module: &shader_module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });

        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            warn!("NV12 GPU converter: shader compilation failed: {err}");
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_params"),
            size: std::mem::size_of::<Nv12Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        info!("NV12 GPU converter initialized");

        Some(Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            y_buf: None,
            uv_buf: None,
            output_buf: None,
            staging_buf: None,
            params_buf,
            cached_y_size: 0,
            cached_uv_size: 0,
            cached_output_size: 0,
        })
    }

    /// Ensure Y plane buffer is large enough.
    fn ensure_y_buf(&mut self, size: u64) {
        if size <= self.cached_y_size {
            return;
        }
        // Round up to multiple of 4 for u32 storage access
        let size = (size + 3) & !3;
        self.y_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_y"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.cached_y_size = size;
    }

    /// Ensure UV plane buffer is large enough.
    fn ensure_uv_buf(&mut self, size: u64) {
        if size <= self.cached_uv_size {
            return;
        }
        let size = (size + 3) & !3;
        self.uv_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_uv"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.cached_uv_size = size;
    }

    /// Ensure output + staging buffers are large enough for RGBA output.
    fn ensure_output_buf(&mut self, size: u64) {
        if size <= self.cached_output_size {
            return;
        }
        self.output_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_out"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_staging"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.cached_output_size = size;
    }

    /// Convert NV12 frame to RGBA with optional resize on the GPU.
    ///
    /// - `y_data`: Y plane bytes (src_height rows, each y_stride bytes)
    /// - `uv_data`: interleaved UV plane bytes (src_height/2 rows, each uv_stride bytes)
    /// - `src_width`, `src_height`: source frame dimensions
    /// - `y_stride`, `uv_stride`: byte stride per row for each plane
    /// - `dst_width`, `dst_height`: desired output dimensions
    ///
    /// Returns the RGBA pixel data (dst_width * dst_height * 4 bytes).
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_to_rgba(
        &mut self,
        y_data: &[u8],
        uv_data: &[u8],
        src_width: u32,
        src_height: u32,
        y_stride: u32,
        uv_stride: u32,
        dst_width: u32,
        dst_height: u32,
    ) -> Vec<u8> {
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;

        self.ensure_y_buf(y_size);
        self.ensure_uv_buf(uv_size);
        self.ensure_output_buf(output_size);

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();
        let output_buf = self.output_buf.as_ref().unwrap();

        // Upload planes
        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        // Write params
        let params = Nv12Params {
            src_width,
            src_height,
            dst_width,
            dst_height,
            y_stride,
            uv_stride,
            _pad0: 0,
            _pad1: 0,
        };
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        // Create bind group
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: y_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uv_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.params_buf.as_entire_binding(),
                },
            ],
        });

        // Dispatch compute shader with 2D workgroups (16×16 threads)
        let workgroups_x = dst_width.div_ceil(16);
        let workgroups_y = dst_height.div_ceil(16);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }

        // Copy output to staging buffer for readback
        let staging = self.staging_buf.as_ref().unwrap();
        encoder.copy_buffer_to_buffer(output_buf, 0, staging, 0, output_size);
        self.queue.submit(std::iter::once(encoder.finish()));

        // Map and read back
        let buffer_slice = staging.slice(..output_size);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);

        match rx.recv() {
            Ok(Ok(())) => {
                let data = buffer_slice.get_mapped_range();
                let result = data.to_vec();
                drop(data);
                staging.unmap();
                result
            }
            _ => {
                warn!("NV12 GPU converter: readback failed, returning zeros");
                vec![0u8; output_size as usize]
            }
        }
    }
}
