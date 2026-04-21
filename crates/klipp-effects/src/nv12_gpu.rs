//! GPU-accelerated NV12 → RGBA conversion with optional resize.
//!
//! Uses a standalone wgpu device so it can run on the decoder thread
//! without sharing the main effects pipeline's device/queue.
//! This replaces the CPU-bound swscale path for hardware-decoded frames.

use tracing::{info, warn};

/// NV12 → RGBA GPU converter. Owns its own wgpu device + queue.
///
/// Device and queue are `Arc`-wrapped so consumers that want to hold a
/// conversion's output buffer on-GPU (`convert_nv12_to_rgba_on_gpu`)
/// can extend the lifetime across threads without cloning the
/// underlying wgpu handles.
pub struct GpuNv12Converter {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
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
    rotation: u32, // 0, 90, 180, 270
    _pad1: u32,
}

/// The compute shader source — NV12 to RGBA with BT.709 color matrix, nearest-neighbor resize,
/// and source rotation (0/90/180/270 degrees).
///
/// Source rotation is applied here during NV12→RGBA conversion (Option A).
/// This is zero-cost: rotation happens in the same GPU pass as color space conversion.
///
/// Alternative: Option C would apply rotation as a separate GPU effect in the
/// effects pipeline (gpu.rs). This would require making GpuEffectsPipeline
/// dimension-aware (separate src/dst dimensions for all ping-pong buffers),
/// which is a larger refactor. Option C would be needed if rotation must
/// interact with other effects that depend on specific orientation, or if
/// rotation needs to be animated via keyframes.
///
/// Uses 2D workgroups (16×16) for spatial locality on the output image.
const SHADER_NV12_TO_RGBA: &str = "
// NV12 to RGBA conversion with optional resize and source rotation
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
    rotation: u32,
    _pad1: u32,
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = gid.y;
    if (dx >= params.dst_width || dy >= params.dst_height) { return; }

    // Map output pixel to source pixel with rotation and nearest-neighbor resize.
    // For 90/270 rotation, dst_width/dst_height are swapped relative to src dimensions
    // (caller is responsible for providing the rotated output dimensions).
    var sx: u32;
    var sy: u32;

    if params.rotation == 0u {
        sx = dx * params.src_width / params.dst_width;
        sy = dy * params.src_height / params.dst_height;
    } else if params.rotation == 90u {
        sx = dy * params.src_width / params.dst_height;
        sy = (params.dst_width - 1u - dx) * params.src_height / params.dst_width;
    } else if params.rotation == 180u {
        sx = (params.dst_width - 1u - dx) * params.src_width / params.dst_width;
        sy = (params.dst_height - 1u - dy) * params.src_height / params.dst_height;
    } else { // 270
        sx = (params.dst_height - 1u - dy) * params.src_width / params.dst_height;
        sy = dx * params.src_height / params.dst_width;
    }

    // Clamp to source bounds
    sx = min(sx, params.src_width - 1u);
    sy = min(sy, params.src_height - 1u);

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
    /// Create a new GPU NV12 converter. Shares the process-wide
    /// `GpuContext` with every other GPU-using type in the engine
    /// (the effects pipeline and the P010 converter), so buffers it
    /// produces can be bound directly as grade-chain inputs without
    /// a device-to-device copy.
    ///
    /// Returns `None` if no suitable GPU adapter exists (falls back
    /// to CPU conversion at the caller). When the shared context's
    /// initial setup has already failed for this process, this will
    /// not retry — see `GpuContext::shared`.
    pub fn new() -> Option<Self> {
        let ctx = crate::gpu_context::GpuContext::shared()?;
        Self::with_context(ctx)
    }

    /// Create an NV12 converter that reuses an existing `GpuContext`.
    /// The decoder thread holds this converter; buffers it produces
    /// via [`Self::convert_nv12_to_rgba_on_gpu`] live on `ctx.device`
    /// and can be bound directly as compute-shader inputs by any
    /// other pipeline built on the same context (phase 4's grade
    /// chain).
    pub fn with_context(ctx: &crate::gpu_context::GpuContext) -> Option<Self> {
        let device = ctx.device.clone();
        let queue = ctx.queue.clone();

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

    /// Clone the Arc-wrapped wgpu device. Callers constructing a
    /// `GpuFrame` from `convert_nv12_to_rgba_on_gpu` output pair this
    /// with `queue()`.
    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Clone the Arc-wrapped wgpu queue. See [`Self::device`].
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
        std::sync::Arc::clone(&self.queue)
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

    /// Convert NV12 frame to RGBA with optional resize and rotation on the GPU.
    ///
    /// - `y_data`: Y plane bytes (src_height rows, each y_stride bytes)
    /// - `uv_data`: interleaved UV plane bytes (src_height/2 rows, each uv_stride bytes)
    /// - `src_width`, `src_height`: source frame dimensions
    /// - `y_stride`, `uv_stride`: byte stride per row for each plane
    /// - `dst_width`, `dst_height`: desired output dimensions (caller must swap for 90°/270°)
    /// - `rotation`: source rotation in degrees (0, 90, 180, 270)
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
        rotation: u32,
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
            rotation,
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

        // Map + poll-loop + read. `Maintain::Wait` and
        // `WaitForSubmissionIndex` both unreliably deadlock on a
        // device shared with a busy UI thread — see
        // `gpu.rs::poll_until_mapped` for the writeup. Inline the
        // same polling pattern here.
        let buffer_slice = staging.slice(..output_size);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let map_result = {
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(2);
            loop {
                self.device.poll(wgpu::Maintain::Poll);
                match rx.try_recv() {
                    Ok(v) => break Some(v),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        if start.elapsed() >= timeout { break None; }
                        std::thread::sleep(std::time::Duration::from_micros(100));
                    }
                }
            }
        };

        match map_result {
            Some(Ok(())) => {
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

    /// GPU-resident variant of [`Self::convert_nv12_to_rgba`]: runs
    /// the NV12→RGBA compute shader and returns the output
    /// `wgpu::Buffer` directly, without downloading to CPU. The
    /// returned buffer is freshly allocated per call (so holding it
    /// across subsequent calls is safe — no internal aliasing) and
    /// flagged `STORAGE | COPY_SRC` so it can both feed another
    /// compute shader (phase 4's grade chain) and be copied into a
    /// staging buffer for readback via `Frame::into_cpu()`.
    ///
    /// Pair with [`Self::device`] / [`Self::queue`] to assemble a
    /// `klipp_engine::GpuFrame` — that's the canonical way to hand a
    /// GPU-resident frame across the engine boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_to_rgba_on_gpu(
        &mut self,
        y_data: &[u8],
        uv_data: &[u8],
        src_width: u32,
        src_height: u32,
        y_stride: u32,
        uv_stride: u32,
        dst_width: u32,
        dst_height: u32,
        rotation: u32,
    ) -> std::sync::Arc<wgpu::Buffer> {
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;

        self.ensure_y_buf(y_size);
        self.ensure_uv_buf(uv_size);

        // Fresh output buffer per call — we're handing ownership to
        // the caller, so we can't reuse the internal cache.
        let output_buf = std::sync::Arc::new(self.device.create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("nv12_out_gpu_resident"),
                size: output_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        ));

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();

        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        let params = Nv12Params {
            src_width,
            src_height,
            dst_width,
            dst_height,
            y_stride,
            uv_stride,
            rotation,
            _pad1: 0,
        };
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

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
        self.queue.submit(std::iter::once(encoder.finish()));

        output_buf
    }
}
