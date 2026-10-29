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
    /// Packed RGB10 code values for scene-referred sources (camera log,
    /// and the rare 8-bit PQ/HLG) on the CPU-readback path.
    hdr_buf: Option<wgpu::Buffer>,
    hdr_staging_buf: Option<wgpu::Buffer>,
    /// Bound as the codes output on display-referred frames, which never
    /// write it.
    hdr_dummy_buf: wgpu::Buffer,
    /// Bound as the linear output whenever the caller wants the sRGB-u8
    /// hand-off instead, which never writes it.
    linear_dummy_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    // Cached dimensions to avoid re-creating buffers
    cached_y_size: u64,
    cached_uv_size: u64,
    cached_output_size: u64,
    /// The GPU-resident outputs handed to callers, kept to be handed out
    /// again once no caller holds them (#2107).
    resident_rgba: ResidentPool,
    resident_codes: ResidentPool,
}

/// Buffers of one size handed out as `Arc`s and taken back when nothing
/// else holds them (#2107). A 4K frame's output was a fresh 33 MB buffer
/// for every frame of every stream, made and freed again a moment later.
/// One handed out again is written by a pass submitted on the same queue
/// after the passes of whoever read it last, so it is never written
/// under them; and only once it has been free for a few frames, so a
/// reader that let go of it before submitting its read has submitted it.
pub(crate) struct ResidentPool {
    label: &'static str,
    size: u64,
    /// Each buffer and the last take at which something else held it.
    buffers: Vec<(std::sync::Arc<wgpu::Buffer>, u64)>,
    takes: u64,
}

impl ResidentPool {
    /// A queue's frames, the frames held while they are shown and a few
    /// in flight. Past it a buffer is made fresh, as before, and not kept.
    const CAP: usize = 24;
    /// Takes a buffer must stay free for before it is handed out again.
    const QUIET_TAKES: u64 = 4;

    pub(crate) fn new(label: &'static str) -> Self {
        Self { label, size: 0, buffers: Vec::new(), takes: 0 }
    }

    /// A `size`-byte buffer nothing else holds: one kept, or one made.
    pub(crate) fn take(&mut self, device: &wgpu::Device, size: u64) -> std::sync::Arc<wgpu::Buffer> {
        if size != self.size {
            self.buffers.clear();
            self.size = size;
        }
        self.takes += 1;
        let now = self.takes;
        for (buffer, held_at) in &mut self.buffers {
            if std::sync::Arc::strong_count(buffer) > 1 {
                *held_at = now;
            }
        }
        if let Some((free, held_at)) = self.buffers.iter_mut().find(|(_, held_at)| now - *held_at >= Self::QUIET_TAKES) {
            *held_at = now;
            return std::sync::Arc::clone(free);
        }
        let made = std::sync::Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        if self.buffers.len() < Self::CAP {
            self.buffers.push((std::sync::Arc::clone(&made), now));
        }
        made
    }

    /// How many buffers it keeps. For tests.
    pub(crate) fn kept(&self) -> usize {
        self.buffers.len()
    }
}

/// What the GPU-resident NV12 conversion produced — the same shape as the
/// P010 converter's, so the decoder treats both alike.
pub type Nv12GpuOutput = crate::p010_gpu::P010GpuOutput;

/// Params uniform for the NV12→RGBA shader (must match WGSL struct layout).
///
/// 12 u32s = 48 bytes, 16-byte aligned — fits the WGSL uniform
/// alignment rules without padding.
///
/// `primaries_matrix` carries the pre-computed source-primaries →
/// working-space matrix as 3 rows × `vec4<f32>` (12 floats; the 4th
/// component of each row is padding for WGSL's `vec3<f32>` alignment).
/// The engine builds it via
/// `klipp_color::camera_log::primaries_to_working_matrix(primaries,
/// working_colorspace)` and passes it through `pack_primaries_matrix`.
/// Sliding the working-space choice into the matrix keeps the shader
/// simple — one matmul per pixel regardless of which working space
/// the project is in.
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
    /// `[range, matrix_id, transfer_id, primaries_id]` from
    /// `klipp_core::color_ids::ids(SourceColorspace)`. Drives the
    /// shader's branching for limited/full range, BT.709/BT.601/
    /// BT.2020 matrix, and EOTF selection. `primaries_id` is no
    /// longer consumed by the shader — gamut conversion now goes
    /// through `primaries_matrix` — but the field is preserved on
    /// the API boundary for future per-primaries dispatch (e.g.
    /// gamut-mapping modes that DO need to know the source identity).
    color_range: u32,
    matrix_id: u32,
    transfer_id: u32,
    primaries_id: u32,
    /// Write linear working-space f32 to the `linear` binding instead
    /// of sRGB-u8 to `output`. The shader computes the linear value
    /// either way — this only decides which of them is thrown away.
    ///
    /// The u8 hand-off is right for the viewer, which is showing an
    /// 8-bit picture anyway. It is wrong for a render: quantising to
    /// 8-bit RGB here costs real precision, because YUV→RGB is a
    /// matrix and its output does not fit back into 8 bits.
    write_linear: u32,
    /// Source-primaries → working-space matrix, packed as 3 rows ×
    /// vec4 (12 floats). Row 0 is dotted against (R, G, B, _) to
    /// produce out.R, etc.
    primaries_matrix: [[f32; 4]; 3],
}

/// Per-shader code that runs after the shared colourspace prelude and
/// helpers ([`crate::color_wgsl::WGSL_PRELUDE`] plus
/// [`crate::color_wgsl::WGSL_HELPERS`]). Reads NV12 (8-bit) Y/UV
/// planes, dispatches the colourspace stages from the shared
/// helpers, re-encodes as sRGB-u8 for the existing CPU/GPU
/// hand-off in `klipp-engine::video_decoder::rgba_u8_to_linear`.
///
/// Source rotation is folded into the same pass — zero-cost since
/// it just remaps the source coordinate before sampling.
const SHADER_NV12_BODY: &str = include_str!(concat!(env!("OUT_DIR"), "/shaders/decode/nv12_body.wgsl"));

/// WGSL shared by the NV12 and P010 decode shaders for a downscale (#744):
/// where an output pixel's footprint lies in the source, through the
/// rotation, and how many samples across it to average.
///
/// The nearest-neighbour lookup each shader keeps for 1:1 and upscales
/// takes one source pixel per output pixel. Shrinking 4K to 1080 that
/// way drops three pixels in four, and fine detail aliases: the viewer
/// shimmered on it, and the export could not use this shader at all and
/// went back through the CPU. A downscale instead averages an `n x m`
/// grid of samples spread evenly over the footprint, in linear light: a
/// box filter at integer ratios, close to an area filter otherwise. The
/// grid is capped at 8 a side, so a thumbnail of a 4K frame stays cheap.
///
/// An upscale is Lanczos-3 (#2557), the kernel swscale's export path used,
/// over 6 x 6 source pixels: a 1080p source in a 4K export was refused here
/// for nearest's blockiness, and went down to the CPU to be scaled there,
/// at 44 ms a frame. Unlike the downscale it filters the sRGB-encoded
/// values, as swscale does, not linear light: on a test chart that matches
/// swscale's export to a level (as 1:1 does), where in linear light hard
/// edges came out up to 61 levels off it, and a Lanczos kernel's ringing
/// round a hard edge looks worse in linear light besides.
pub(crate) const RESIZE_WGSL: &str = include_str!(concat!(env!("OUT_DIR"), "/shaders/decode/resize_body.wgsl"));

/// Full shader source: shared prelude + helpers + the pixel storage hook
/// + NV12-specific body.
///
/// The storage hook is here because the `linear` output is a frame buffer
/// in the chain's own format — it gets copied into `buf_a` — so its store
/// has to go through `pack` like every other pixel write in the codebase.
/// It is assembled at runtime from three pieces; [`nv12_shader`] builds
/// it once, and `gpu::shader_sources` lists that, so the WGSL validation
/// and `pixel_access_goes_through_pack` check it like the rest.
fn nv12_shader_source() -> String {
    let mut s = String::with_capacity(
        crate::color_wgsl::WGSL_PRELUDE.len()
            + crate::color_wgsl::WGSL_HELPERS.len()
            + SHADER_NV12_BODY.len()
            + RESIZE_WGSL.len()
            + 256,
    );
    s.push_str(crate::color_wgsl::WGSL_PRELUDE);
    s.push_str(crate::color_wgsl::WGSL_HELPERS);
    s.push_str(crate::gpu::shader_pixel_storage_source());
    s.push_str(RESIZE_WGSL);
    s.push_str(SHADER_NV12_BODY);
    s
}

/// [`nv12_shader_source`], built once.
pub(crate) fn nv12_shader() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(nv12_shader_source)
}

impl GpuNv12Converter {
    /// How many GPU-resident output buffers the converter keeps to hand
    /// out again (#2107). For tests.
    #[doc(hidden)]
    pub fn resident_buffers_kept(&self) -> usize {
        self.resident_rgba.kept()
    }

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
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Linear working-space output.
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
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
            source: wgpu::ShaderSource::Wgsl(nv12_shader().into()),
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
            crate::shader_check::shader_failed("NV12 converter", &err.to_string());
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_params"),
            size: std::mem::size_of::<Nv12Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let hdr_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_hdr_dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let linear_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_linear_dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
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
            hdr_buf: None,
            hdr_staging_buf: None,
            hdr_dummy_buf,
            linear_dummy_buf,
            params_buf,
            cached_y_size: 0,
            cached_uv_size: 0,
            cached_output_size: 0,
            resident_rgba: ResidentPool::new("nv12_out_gpu_resident"),
            resident_codes: ResidentPool::new("nv12_hdr_codes_gpu_resident"),
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
    /// - `color_ids`: `[range, matrix_id, transfer_id, primaries_id]`
    ///   from `klipp_core::color_ids::ids(SourceColorspace)`.
    /// - `primaries_matrix`: source-primaries → working-space 3x3,
    ///   row-major. Pass [`crate::color_wgsl::pack_primaries_matrix`]
    ///   the output of `klipp_color::camera_log::primaries_to_working_matrix`
    ///   to get the working-space matrix; the helper packs to the
    ///   3-row vec4 layout the shader expects. For pure BT.709 SDR
    ///   sources targeting `Rec709Linear` working space, identity
    ///   ([`crate::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED`])
    ///   is correct.
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
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> Vec<u8> {
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;

        self.ensure_y_buf(y_size);
        self.ensure_uv_buf(uv_size);
        self.ensure_output_buf(output_size);
        // Scene-referred sources write codes too; give them a real buffer
        // so nothing is written out of bounds.
        let hdr = crate::p010_gpu::is_hdr_transfer(color_ids[2]);
        if hdr && self.hdr_buf.as_ref().is_none_or(|b| b.size() < output_size) {
            self.hdr_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nv12_hdr"),
                size: output_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
        }

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();
        let output_buf = self.output_buf.as_ref().unwrap();
        let hdr_binding = if hdr { self.hdr_buf.as_ref().unwrap() } else { &self.hdr_dummy_buf };

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
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 0,
            primaries_matrix,
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
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: hdr_binding.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.linear_dummy_buf.as_entire_binding(),
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
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Nv12Convert);
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
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Idle);

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
        // `color_ids` is `[range, matrix_id, transfer_id, primaries_id]`.
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> std::sync::Arc<wgpu::Buffer> {
        self.convert_nv12_on_gpu(
            y_data, uv_data, src_width, src_height, y_stride, uv_stride,
            dst_width, dst_height, rotation, color_ids, primaries_matrix,
        )
        .rgba
    }

    /// GPU-resident conversion returning the u8 buffer and, for
    /// scene-referred sources (camera log, PQ, HLG), the packed RGB10 code
    /// values the grade reads. Both freshly allocated for the caller.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_on_gpu(
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
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> Nv12GpuOutput {
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;

        self.ensure_y_buf(y_size);
        self.ensure_uv_buf(uv_size);
        let hdr_codes = crate::p010_gpu::is_hdr_transfer(color_ids[2]).then(|| self.resident_codes.take(&self.device, output_size));
        let hdr_binding: &wgpu::Buffer = match hdr_codes.as_ref() {
            Some(b) => b,
            None => &self.hdr_dummy_buf,
        };

        // The caller's to hold: a buffer nothing else holds now, from the
        // pool, not the internal cache the readback paths share.
        let output_buf = self.resident_rgba.take(&self.device, output_size);

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
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 0,
            primaries_matrix,
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
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: hdr_binding.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.linear_dummy_buf.as_entire_binding(),
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

        Nv12GpuOutput { rgba: output_buf, hdr_codes }
    }

    /// [`Self::convert_nv12_on_gpu`] at full precision: the frame lands as
    /// linear working-space f32 rather than sRGB u8, ready to be a grade
    /// chain's input slot.
    ///
    /// This is the render's route. The u8 hand-off is the right shape for
    /// the viewer, which is showing eight bits anyway, but it quantises —
    /// YUV→RGB is a matrix and its output does not fit back into 8 bits —
    /// and a delivered frame should not pay that. The shader computes the
    /// linear value either way; `write_linear` only decides which one is
    /// discarded.
    ///
    /// The buffer comes back on **this converter's device**, so a caller
    /// that means to hand it to the grade chain must have built the
    /// converter with [`Self::with_context`] on the same context the chain
    /// uses. Rotation and resize happen here, in the same pass, so the
    /// caller does not need a separate CPU pass for either.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_to_linear_on_gpu(
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
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> std::sync::Arc<wgpu::Buffer> {
        self.ensure_y_buf(y_data.len() as u64);
        self.ensure_uv_buf(uv_data.len() as u64);

        // 16 bytes a pixel, and the caller takes ownership, so this is
        // not the internal cache.
        let linear_buf = std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_linear_gpu_resident"),
            size: (dst_width as u64) * (dst_height as u64) * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));

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
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 1,
            primaries_matrix,
        };
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12_to_linear_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: y_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: uv_buf.as_entire_binding() },
                // The u8 output is the one going unwritten this time.
                wgpu::BindGroupEntry { binding: 2, resource: self.linear_dummy_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: self.hdr_dummy_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: linear_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("nv12_to_linear") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(dst_width.div_ceil(16), dst_height.div_ceil(16), 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        linear_buf
    }

    /// Convert one scene-referred NV12 frame and read back its packed RGB10
    /// code values. `None` for a display-referred transfer or a failed
    /// readback, so the caller can fall back to `convert_nv12_to_rgba`.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_hdr_codes(
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
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> Option<Vec<u32>> {
        if !crate::p010_gpu::is_hdr_transfer(color_ids[2]) {
            return None;
        }
        let out = self.convert_nv12_on_gpu(
            y_data, uv_data, src_width, src_height, y_stride, uv_stride,
            dst_width, dst_height, rotation, color_ids, primaries_matrix,
        );
        let codes = out.hdr_codes?;
        let size = codes.size();
        if self.hdr_staging_buf.as_ref().is_none_or(|b| b.size() < size) {
            self.hdr_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nv12_hdr_staging"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let staging = self.hdr_staging_buf.as_ref().unwrap();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("nv12_hdr_readback"),
        });
        encoder.copy_buffer_to_buffer(&codes, 0, staging, 0, size);
        self.queue.submit(std::iter::once(encoder.finish()));
        let slice = staging.slice(..size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        match crate::gpu::poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let words = bytemuck::cast_slice::<u8, u32>(&slice.get_mapped_range()).to_vec();
                staging.unmap();
                Some(words)
            }
            _ => {
                warn!("NV12 GPU converter: code readback failed");
                None
            }
        }
    }
}
