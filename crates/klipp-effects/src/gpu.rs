//! GPU-accelerated effects via wgpu compute shaders.
//!
//! Each effect runs as a compute shader dispatch on the GPU.
//! Frames are uploaded, processed through the effect chain, and downloaded.
//! Falls back to CPU if GPU is unavailable.

use crate::BuiltinEffect;
use tracing::{debug, info, warn};

/// GPU effects pipeline — holds device, queue, compute pipelines, and reusable buffers.
pub struct GpuEffectsPipeline {
    device: wgpu::Device,
    queue: wgpu::Queue,
    // Compute pipelines
    opacity_pipeline: wgpu::ComputePipeline,
    blur_h_pipeline: wgpu::ComputePipeline,
    blur_v_pipeline: wgpu::ComputePipeline,
    sharpen_pipeline: wgpu::ComputePipeline,
    crop_pipeline: wgpu::ComputePipeline,
    transform_pipeline: wgpu::ComputePipeline,
    chroma_key_pipeline: wgpu::ComputePipeline,
    // Shared bind group layout (input, output, params)
    bind_group_layout: wgpu::BindGroupLayout,
    // Reusable buffers — resized when frame dimensions change
    buf_a: Option<wgpu::Buffer>,
    buf_b: Option<wgpu::Buffer>,
    staging_read: Option<wgpu::Buffer>,
    params_buf: wgpu::Buffer,
    current_buf_size: u64,
}

/// Maximum params size (padded to 256 bytes for alignment)
const PARAMS_SIZE: u64 = 256;
const WORKGROUP_SIZE: u32 = 256;

impl GpuEffectsPipeline {
    /// Create a new GPU effects pipeline. Returns `None` if no suitable GPU is found.
    pub fn new() -> Option<Self> {
        // Ensure Vulkan ICD is discoverable (some distros put it in /usr/share not /etc)
        if std::env::var("VK_DRIVER_FILES").is_err() {
            let icd_path = "/usr/share/vulkan/icd.d/nvidia_icd.json";
            if std::path::Path::new(icd_path).exists() {
                std::env::set_var("VK_DRIVER_FILES", icd_path);
                debug!("GPU effects: set VK_DRIVER_FILES={icd_path}");
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
        }));

        let adapter = match adapter {
            Some(a) => a,
            None => {
                warn!("GPU effects: no suitable adapter found — falling back to CPU");
                return None;
            }
        };

        let adapter_info = adapter.get_info();
        info!(
            "GPU effects: {} ({:?})",
            adapter_info.name, adapter_info.backend
        );

        let (device, queue) = match pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("klipp-effects"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            },
            None,
        )) {
            Ok(dq) => dq,
            Err(e) => {
                warn!("GPU effects: device creation failed: {e} — falling back to CPU");
                return None;
            }
        };

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effects_bgl"),
            entries: &[
                // binding 0: input (read-only storage)
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
                // binding 1: output (read-write storage)
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
                // binding 2: params (uniform)
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Push error handler to capture shader validation errors instead of panicking
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let create_pipeline = |dev: &wgpu::Device, label: &str, shader_src: &str| -> wgpu::ComputePipeline {
            let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(shader_src.into()),
            });
            dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let opacity_pipeline = create_pipeline(&device, "opacity", SHADER_OPACITY);
        let blur_h_pipeline = create_pipeline(&device, "blur_h", SHADER_BLUR_H);
        let blur_v_pipeline = create_pipeline(&device, "blur_v", SHADER_BLUR_V);
        let sharpen_pipeline = create_pipeline(&device, "sharpen", SHADER_SHARPEN);
        let crop_pipeline = create_pipeline(&device, "crop", SHADER_CROP);
        let transform_pipeline = create_pipeline(&device, "transform", SHADER_TRANSFORM);
        let chroma_key_pipeline = create_pipeline(&device, "chroma_key", SHADER_CHROMA_KEY);

        // Check if any shader compilation failed
        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            warn!("GPU effects: shader compilation failed: {err} — falling back to CPU");
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        info!("GPU effects pipeline initialized");

        Some(Self {
            device,
            queue,
            opacity_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            sharpen_pipeline,
            crop_pipeline,
            transform_pipeline,
            chroma_key_pipeline,
            bind_group_layout,
            buf_a: None,
            buf_b: None,
            staging_read: None,
            params_buf,
            current_buf_size: 0,
        })
    }

    /// Ensure GPU buffers are large enough for the frame.
    fn ensure_buffers(&mut self, byte_size: u64) {
        if byte_size <= self.current_buf_size {
            return;
        }
        debug!("GPU effects: resizing buffers to {byte_size} bytes");
        self.buf_a = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_a"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.buf_b = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_b"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.staging_read = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_staging_read"),
            size: byte_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.current_buf_size = byte_size;
    }

    /// Create a bind group with the given input/output buffers.
    fn bind_group(&self, input: &wgpu::Buffer, output: &wgpu::Buffer) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params_buf.as_entire_binding(),
                },
            ],
        })
    }

    /// Apply a chain of effects on the GPU. Returns the processed RGBA data.
    pub fn apply_effects(
        &mut self,
        effects: &[BuiltinEffect],
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        if effects.is_empty() {
            return rgba;
        }

        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        // Upload frame data to buf_a
        self.queue.write_buffer(buf_a, 0, &rgba);

        // Track which buffer has the current data (true = buf_a, false = buf_b)
        let mut current_in_a = true;
        let pixel_count = width * height;
        let workgroups = (pixel_count + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;

        for effect in effects {
            let (input, output) = if current_in_a {
                (buf_a, buf_b)
            } else {
                (buf_b, buf_a)
            };

            match effect {
                BuiltinEffect::Opacity { value } => {
                    if (*value - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&OpacityParams {
                        width,
                        height,
                        value: *value,
                        _pad: 0,
                    });
                    self.dispatch(&self.opacity_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::GaussianBlur { radius } => {
                    if *radius < 0.5 {
                        continue;
                    }
                    // 3-pass box blur to approximate Gaussian
                    let boxes = boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        if box_r == 0 {
                            continue;
                        }
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        // Horizontal pass
                        self.write_params(&BlurParams {
                            width,
                            height,
                            radius: box_r as i32,
                            _pad: 0,
                        });
                        self.dispatch(&self.blur_h_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;

                        // Vertical pass
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        self.dispatch(&self.blur_v_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;
                    }
                }
                BuiltinEffect::Sharpen { amount } => {
                    if amount.abs() < f32::EPSILON {
                        continue;
                    }
                    // Single-pass 3x3 unsharp mask — the shader samples neighbors directly
                    self.write_params(&SharpenParams {
                        width,
                        height,
                        amount: *amount,
                        _pad: 0,
                    });
                    self.dispatch(&self.sharpen_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Crop {
                    left,
                    right,
                    top,
                    bottom,
                    feather,
                } => {
                    if *left <= 0.0 && *right <= 0.0 && *top <= 0.0 && *bottom <= 0.0 {
                        continue;
                    }
                    self.write_params(&CropParams {
                        width,
                        height,
                        crop_left: *left * width as f32,
                        crop_right: (1.0 - *right) * width as f32,
                        crop_top: *top * height as f32,
                        crop_bottom: (1.0 - *bottom) * height as f32,
                        feather: *feather,
                        _pad: 0,
                    });
                    self.dispatch(&self.crop_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Transform {
                    position_x,
                    position_y,
                    scale_x,
                    scale_y,
                    rotation,
                    anchor_x,
                    anchor_y,
                } => {
                    if *position_x == 0.0
                        && *position_y == 0.0
                        && (*scale_x - 1.0).abs() < f32::EPSILON
                        && (*scale_y - 1.0).abs() < f32::EPSILON
                        && rotation.abs() < f32::EPSILON
                    {
                        continue;
                    }
                    let rot_rad = rotation.to_radians();
                    let cos_r = rot_rad.cos();
                    let sin_r = rot_rad.sin();
                    let inv_sx = if scale_x.abs() > f32::EPSILON {
                        1.0 / scale_x
                    } else {
                        1.0
                    };
                    let inv_sy = if scale_y.abs() > f32::EPSILON {
                        1.0 / scale_y
                    } else {
                        1.0
                    };
                    self.write_params(&TransformParams {
                        width,
                        height,
                        anchor_x: *anchor_x * width as f32,
                        anchor_y: *anchor_y * height as f32,
                        position_x: *position_x,
                        position_y: *position_y,
                        cos_r,
                        sin_r,
                        inv_sx,
                        inv_sy,
                        _pad: [0; 2],
                    });
                    self.dispatch(&self.transform_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::ChromaKey {
                    key_color,
                    tolerance,
                    spill_suppression,
                } => {
                    // Convert key color to CbCr
                    let rf = key_color[0] as f32;
                    let gf = key_color[1] as f32;
                    let bf = key_color[2] as f32;
                    let key_cb = 128.0 + (-0.168736 * rf - 0.331264 * gf + 0.5 * bf);
                    let key_cr = 128.0 + (0.5 * rf - 0.418688 * gf - 0.081312 * bf);

                    self.write_params(&ChromaKeyParams {
                        width,
                        height,
                        key_cb,
                        key_cr,
                        tolerance: *tolerance,
                        spill_suppression: *spill_suppression,
                        _pad: [0; 2],
                    });
                    self.dispatch(&self.chroma_key_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
            }
        }

        // Download result from GPU
        self.download(
            if current_in_a { buf_a } else { buf_b },
            byte_size,
        )
    }

    fn write_params<T: bytemuck::Pod>(&self, data: &T) {
        let bytes = bytemuck::bytes_of(data);
        self.queue.write_buffer(&self.params_buf, 0, bytes);
    }

    fn dispatch(
        &self,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        workgroups: u32,
    ) {
        let bind_group = self.bind_group(input, output);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    fn download(&self, source: &wgpu::Buffer, byte_size: u64) -> Vec<u8> {
        let staging = self.staging_read.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(source, 0, staging, 0, byte_size);
        self.queue.submit(std::iter::once(encoder.finish()));

        let buffer_slice = staging.slice(..byte_size);
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
                warn!("GPU effects: download failed, returning zeros");
                vec![0u8; byte_size as usize]
            }
        }
    }
}

// ─── Param structs (must be Pod for bytemuck) ────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OpacityParams {
    width: u32,
    height: u32,
    value: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SharpenParams {
    width: u32,
    height: u32,
    amount: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CropParams {
    width: u32,
    height: u32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    feather: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformParams {
    width: u32,
    height: u32,
    anchor_x: f32,
    anchor_y: f32,
    position_x: f32,
    position_y: f32,
    cos_r: f32,
    sin_r: f32,
    inv_sx: f32,
    inv_sy: f32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaKeyParams {
    width: u32,
    height: u32,
    key_cb: f32,
    key_cr: f32,
    tolerance: f32,
    spill_suppression: f32,
    _pad: [u32; 2],
}

// ─── Box blur Gaussian approximation (same as CPU version) ───────────────────

fn boxes_for_gaussian(sigma: f32) -> [usize; 3] {
    let w_ideal = ((12.0 * sigma * sigma / 3.0) + 1.0).sqrt();
    let mut wl = w_ideal.floor() as usize;
    if wl % 2 == 0 {
        wl = wl.saturating_sub(1).max(1);
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma
        - 3.0 * (wl * wl) as f32
        - 12.0 * wl as f32
        - 9.0)
        / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as usize;
    let r = |size: usize| (size - 1) / 2;
    [
        r(if 0 < m { wl } else { wu }),
        r(if 1 < m { wl } else { wu }),
        r(if 2 < m { wl } else { wu }),
    ]
}

// ─── WGSL Compute Shaders ────────────────────────────────────────────────────

const SHADER_OPACITY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    value: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = unpack(input[idx]);
    output[idx] = pack(pixel * params.value);
}
"
);

const SHADER_BLUR_H: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let w_i32 = i32(params.width);

    for (var dx = -params.radius; dx <= params.radius; dx++) {
        let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
        sum += unpack(input[y * params.width + u32(sx)]);
    }
    output[idx] = pack(sum * inv);
}
"
);

const SHADER_BLUR_V: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let h_i32 = i32(params.height);

    for (var dy = -params.radius; dy <= params.radius; dy++) {
        let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
        sum += unpack(input[u32(sy) * params.width + x]);
    }
    output[idx] = pack(sum * inv);
}
"
);

const SHADER_SHARPEN: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let orig = unpack(input[idx]);

    // 3x3 average of neighbors (excluding center)
    var avg = vec4<f32>(0.0);
    let w_i32 = i32(params.width);
    let h_i32 = i32(params.height);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
            let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
            avg += unpack(input[u32(sy) * params.width + u32(sx)]);
        }
    }
    avg = avg / 9.0;

    // Unsharp mask: original + amount * (original - blurred)
    let sharpened = vec4<f32>(
        orig.x + params.amount * (orig.x - avg.x),
        orig.y + params.amount * (orig.y - avg.y),
        orig.z + params.amount * (orig.z - avg.z),
        orig.w,  // preserve alpha
    );
    output[idx] = pack(sharpened);
}
"
);

const SHADER_CROP: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    feather: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let xf = f32(x);
    let yf = f32(y);

    if xf < params.crop_left || xf >= params.crop_right || yf < params.crop_top || yf >= params.crop_bottom {
        output[idx] = 0u;
        return;
    }

    var pixel = unpack(input[idx]);

    if params.feather > 0.0 {
        let dist = min(
            min(xf - params.crop_left, params.crop_right - 1.0 - xf),
            min(yf - params.crop_top, params.crop_bottom - 1.0 - yf),
        );
        if dist < params.feather {
            let factor = dist / params.feather;
            pixel.w = pixel.w * factor;
        }
    }

    output[idx] = pack(pixel);
}
"
);

const SHADER_TRANSFORM: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    anchor_x: f32,
    anchor_y: f32,
    position_x: f32,
    position_y: f32,
    cos_r: f32,
    sin_r: f32,
    inv_sx: f32,
    inv_sy: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    // Inverse mapping: output pixel → source pixel
    let dx = f32(x) - params.anchor_x - params.position_x;
    let dy = f32(y) - params.anchor_y - params.position_y;

    // Inverse rotation
    let rx = dx * params.cos_r + dy * params.sin_r;
    let ry = -dx * params.sin_r + dy * params.cos_r;

    // Inverse scale
    let sx = rx * params.inv_sx + params.anchor_x;
    let sy = ry * params.inv_sy + params.anchor_y;

    // Nearest-neighbour sample
    let src_x = i32(round(sx));
    let src_y = i32(round(sy));

    if src_x >= 0 && src_x < i32(params.width) && src_y >= 0 && src_y < i32(params.height) {
        let src_idx = u32(src_y) * params.width + u32(src_x);
        output[idx] = input[src_idx];
    } else {
        output[idx] = 0u;  // transparent
    }
}
"
);

const SHADER_CHROMA_KEY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    key_cb: f32,
    key_cr: f32,
    tolerance: f32,
    spill_suppression: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
fn rgb_to_cbcr(r: f32, g: f32, b: f32) -> vec2<f32> {
    let cb = 128.0 + (-0.168736 * r - 0.331264 * g + 0.5 * b);
    let cr = 128.0 + (0.5 * r - 0.418688 * g - 0.081312 * b);
    return vec2<f32>(cb, cr);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    var pixel = unpack(input[idx]);
    let cbcr = rgb_to_cbcr(pixel.x, pixel.y, pixel.z);

    let dcb = (cbcr.x - params.key_cb) / 255.0;
    let dcr = (cbcr.y - params.key_cr) / 255.0;
    let dist = sqrt(dcb * dcb + dcr * dcr);

    if dist < params.tolerance {
        pixel = vec4<f32>(0.0);
    } else if dist < params.tolerance + params.spill_suppression {
        let factor = (dist - params.tolerance) / params.spill_suppression;
        pixel.w = pixel.w * factor;
    }

    output[idx] = pack(pixel);
}
"
);
