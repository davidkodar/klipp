//! GPU-accelerated scope computation using wgpu compute shaders.
//!
//! Provides a lightweight pipeline that computes histogram, waveform, and parade
//! data on the GPU using atomic operations.

use tracing::info;

/// GPU-computed histogram data (matches klipp_color::scopes::HistogramData layout).
pub struct GpuHistogramData {
    pub r: [u32; 256],
    pub g: [u32; 256],
    pub b: [u32; 256],
    pub luma: [u32; 256],
}

/// GPU-computed waveform data (matches klipp_color::scopes::WaveformData layout).
pub struct GpuWaveformData {
    pub bins: Vec<[u32; 256]>,
    pub width: u32,
}

/// GPU-computed parade data (matches klipp_color::scopes::ParadeData layout).
pub struct GpuParadeData {
    pub red: Vec<[u32; 256]>,
    pub green: Vec<[u32; 256]>,
    pub blue: Vec<[u32; 256]>,
    pub width: u32,
}

const WORKGROUP_SIZE: u32 = 256;

pub struct GpuScopesPipeline {
    device: wgpu::Device,
    queue: wgpu::Queue,
    histogram_pipeline: wgpu::ComputePipeline,
    waveform_pipeline: wgpu::ComputePipeline,
    parade_pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    // Reusable buffers
    input_buf: Option<wgpu::Buffer>,
    input_buf_size: u64,
    bins_buf: Option<wgpu::Buffer>,
    bins_buf_size: u64,
    staging_buf: Option<wgpu::Buffer>,
    staging_buf_size: u64,
    params_buf: wgpu::Buffer,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScopeParams {
    width: u32,
    height: u32,
    display_columns: u32,
    step: u32,
}

const SHADER_HISTOGRAM: &str = "
struct Params {
    width: u32,
    height: u32,
    display_columns: u32,
    step: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let total = params.width * params.height;
    if idx >= total { return; }

    let x = idx % params.width;
    let y = idx / params.width;
    if (x % params.step != 0u) || (y % params.step != 0u) { return; }

    let pixel = input[idx];
    let r = pixel & 0xFFu;
    let g = (pixel >> 8u) & 0xFFu;
    let b = (pixel >> 16u) & 0xFFu;

    atomicAdd(&bins[r], 1u);
    atomicAdd(&bins[256u + g], 1u);
    atomicAdd(&bins[512u + b], 1u);

    let luma = u32(clamp(0.2126 * f32(r) + 0.7152 * f32(g) + 0.0722 * f32(b), 0.0, 255.0));
    atomicAdd(&bins[768u + luma], 1u);
}
";

const SHADER_WAVEFORM: &str = "
struct Params {
    width: u32,
    height: u32,
    display_columns: u32,
    step: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let total = params.width * params.height;
    if idx >= total { return; }

    let x = idx % params.width;
    if (x % params.step != 0u) { return; }

    let pixel = input[idx];
    let r = f32(pixel & 0xFFu) / 255.0;
    let g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let luma = clamp(0.2126 * r + 0.7152 * g + 0.0722 * b, 0.0, 1.0);
    let bin = u32(luma * 255.0);
    let col = x * params.display_columns / params.width;

    atomicAdd(&bins[col * 256u + bin], 1u);
}
";

const SHADER_PARADE: &str = "
struct Params {
    width: u32,
    height: u32,
    display_columns: u32,
    step: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let total = params.width * params.height;
    if idx >= total { return; }

    let x = idx % params.width;
    if (x % params.step != 0u) { return; }

    let pixel = input[idx];
    let r = pixel & 0xFFu;
    let g = (pixel >> 8u) & 0xFFu;
    let b = (pixel >> 16u) & 0xFFu;
    let col = x * params.display_columns / params.width;
    let base = col * 256u;

    // R bins: 0..display_columns*256
    // G bins: display_columns*256..display_columns*512
    // B bins: display_columns*512..display_columns*768
    let dc256 = params.display_columns * 256u;
    atomicAdd(&bins[base + r], 1u);
    atomicAdd(&bins[dc256 + base + g], 1u);
    atomicAdd(&bins[dc256 * 2u + base + b], 1u);
}
";

impl GpuScopesPipeline {
    /// Create a new GPU scopes pipeline. Returns `None` if no suitable GPU is found.
    pub fn new() -> Option<Self> {
        // Ensure Vulkan ICD is discoverable
        if std::env::var("VK_DRIVER_FILES").is_err() {
            for path in &[
                "/usr/share/vulkan/icd.d/nvidia_icd.json",
                "/etc/vulkan/icd.d/nvidia_icd.json",
            ] {
                if std::path::Path::new(path).exists() {
                    std::env::set_var("VK_DRIVER_FILES", path);
                    break;
                }
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::from_build_config() & !wgpu::InstanceFlags::VALIDATION,
            ..Default::default()
        });

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))?;

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("gpu_scopes"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            },
            None,
        ))
        .ok()?;

        // Bind group layout: input (read), bins (read_write), params (uniform)
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scope_bgl"),
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
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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

        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scope_pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let create = |label: &str, src: &str| -> wgpu::ComputePipeline {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let histogram_pipeline = create("scope_histogram", SHADER_HISTOGRAM);
        let waveform_pipeline = create("scope_waveform", SHADER_WAVEFORM);
        let parade_pipeline = create("scope_parade", SHADER_PARADE);

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scope_params"),
            size: 256, // padded
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        info!("GPU scopes pipeline initialized");

        Some(Self {
            device,
            queue,
            histogram_pipeline,
            waveform_pipeline,
            parade_pipeline,
            bgl,
            input_buf: None,
            input_buf_size: 0,
            bins_buf: None,
            bins_buf_size: 0,
            staging_buf: None,
            staging_buf_size: 0,
            params_buf,
        })
    }

    fn ensure_input_buf(&mut self, size: u64) {
        if self.input_buf_size < size {
            self.input_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope_input"),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.input_buf_size = size;
        }
    }

    fn ensure_bins_buf(&mut self, num_bins: u64) {
        let size = num_bins * 4; // u32 per bin
        if self.bins_buf_size < size {
            self.bins_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope_bins"),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.bins_buf_size = size;
        }
    }

    fn ensure_staging_buf(&mut self, size: u64) {
        if self.staging_buf_size < size {
            self.staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope_staging"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.staging_buf_size = size;
        }
    }

    fn download_bins(&self, num_bins: usize) -> Vec<u32> {
        let byte_size = (num_bins * 4) as u64;
        let bins_buf = self.bins_buf.as_ref().unwrap();
        let staging = self.staging_buf.as_ref().unwrap();

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(bins_buf, 0, staging, 0, byte_size);
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().ok().and_then(|r| r.ok());

        let data = slice.get_mapped_range();
        let result: Vec<u32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        staging.unmap();

        result
    }

    /// Compute histogram on GPU. Returns the same HistogramData as CPU version.
    pub fn compute_histogram(&mut self, rgba: &[u8], width: u32, height: u32, step: u32) -> GpuHistogramData {
        let frame_bytes = rgba.len() as u64;
        let pixel_count = width * height;
        let num_bins: u64 = 4 * 256; // R + G + B + Luma

        self.ensure_input_buf(frame_bytes);
        self.ensure_bins_buf(num_bins);
        self.ensure_staging_buf(num_bins * 4);

        let input_buf = self.input_buf.as_ref().unwrap();
        let bins_buf = self.bins_buf.as_ref().unwrap();

        self.queue.write_buffer(input_buf, 0, rgba);
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&ScopeParams {
            width, height, display_columns: 0, step: step.max(1),
        }));

        // Clear bins to zero
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.clear_buffer(bins_buf, 0, None);

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.histogram_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::ScopeHistogram);
        let bins = self.download_bins(num_bins as usize);
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Idle);
        let mut data = GpuHistogramData {
            r: [0u32; 256], g: [0u32; 256], b: [0u32; 256], luma: [0u32; 256],
        };
        data.r.copy_from_slice(&bins[0..256]);
        data.g.copy_from_slice(&bins[256..512]);
        data.b.copy_from_slice(&bins[512..768]);
        data.luma.copy_from_slice(&bins[768..1024]);
        data
    }

    /// Compute waveform on GPU. Returns the same WaveformData as CPU version.
    pub fn compute_waveform(&mut self, rgba: &[u8], width: u32, height: u32, display_columns: u32, step: u32) -> GpuWaveformData {
        let display_columns = display_columns.max(1).min(width);
        let frame_bytes = rgba.len() as u64;
        let pixel_count = width * height;
        let num_bins = (display_columns as u64) * 256;

        self.ensure_input_buf(frame_bytes);
        self.ensure_bins_buf(num_bins);
        self.ensure_staging_buf(num_bins * 4);

        let input_buf = self.input_buf.as_ref().unwrap();
        let bins_buf = self.bins_buf.as_ref().unwrap();

        self.queue.write_buffer(input_buf, 0, rgba);
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&ScopeParams {
            width, height, display_columns, step: step.max(1),
        }));

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.clear_buffer(bins_buf, 0, None);

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.waveform_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::ScopeWaveform);
        let flat_bins = self.download_bins(num_bins as usize);
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Idle);
        let mut bins = vec![[0u32; 256]; display_columns as usize];
        for col in 0..display_columns as usize {
            let start = col * 256;
            bins[col].copy_from_slice(&flat_bins[start..start + 256]);
        }
        GpuWaveformData { bins, width: display_columns }
    }

    /// Compute RGB parade on GPU. Returns the same ParadeData as CPU version.
    pub fn compute_parade(&mut self, rgba: &[u8], width: u32, height: u32, display_columns: u32, step: u32) -> GpuParadeData {
        let display_columns = display_columns.max(1).min(width);
        let frame_bytes = rgba.len() as u64;
        let pixel_count = width * height;
        let num_bins = (display_columns as u64) * 256 * 3; // R + G + B

        self.ensure_input_buf(frame_bytes);
        self.ensure_bins_buf(num_bins);
        self.ensure_staging_buf(num_bins * 4);

        let input_buf = self.input_buf.as_ref().unwrap();
        let bins_buf = self.bins_buf.as_ref().unwrap();

        self.queue.write_buffer(input_buf, 0, rgba);
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&ScopeParams {
            width, height, display_columns, step: step.max(1),
        }));

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.clear_buffer(bins_buf, 0, None);

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.parade_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::ScopeParade);
        let flat_bins = self.download_bins(num_bins as usize);
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Idle);
        let dc = display_columns as usize;
        let mut red = vec![[0u32; 256]; dc];
        let mut green = vec![[0u32; 256]; dc];
        let mut blue = vec![[0u32; 256]; dc];
        for col in 0..dc {
            let start = col * 256;
            red[col].copy_from_slice(&flat_bins[start..start + 256]);
            green[col].copy_from_slice(&flat_bins[dc * 256 + start..dc * 256 + start + 256]);
            blue[col].copy_from_slice(&flat_bins[dc * 512 + start..dc * 512 + start + 256]);
        }
        GpuParadeData { red, green, blue, width: display_columns }
    }
}
