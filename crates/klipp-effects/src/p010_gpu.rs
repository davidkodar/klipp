//! GPU P010 (10-bit semi-planar YUV) → sRGB-u8 RGBA conversion.
//!
//! Replaces the CPU `sws_scale` path that costs ~21 ms per 4K frame
//! AND mishandles HDR (decoded PQ samples as if they were sRGB,
//! decoded BT.2020 chroma as if it were BT.709). On a 4K HDR clip the
//! CPU path produced ~10.5 fps with garbage colors. This GPU shader
//! lands the correct image in ~5-8 ms total (sub-ms compute + 33 MB
//! readback).
//!
//! Per pixel, the shader decodes P010 (10-bit Y and interleaved UV, value
//! in the upper bits of each u16), normalises for range and applies the
//! source YUV→RGB matrix. What happens next depends on the transfer:
//!
//! * **SDR** — EOTF, gamut conversion to the working space, sRGB OETF,
//!   quantise to u8. That u8 frame is the grade's input.
//! * **HDR (PQ / HLG)** — the RGB is still PQ/HLG-*encoded*; it is
//!   quantised to 10-bit code values and written, packed RGB10, to a
//!   second output. That buffer — not the u8 one — is the grade's input:
//!   the input transform turns it into scene-linear (1.0 = 100 nits) so a
//!   grade sees the full HDR range. The u8 output becomes a *display
//!   preview* (scene-linear → working primaries → filmic tone map →
//!   sRGB) used only where nothing grades the frame.
//!
//! The HDR preview is computed from the quantised codes, not the
//! pre-quantisation RGB, so a preview frame and a graded-at-identity
//! frame start from byte-identical input. The shared reference for all
//! of this is `klipp_color::hdr`.
//!
//! The tone map used to run here on *every* HDR frame, before any grade —
//! so grades operated on already-compressed highlights, and the
//! InputTransform(PQ) seeded on import then decoded PQ a second time.

use tracing::{info, warn};

pub struct GpuP010Converter {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    y_buf: Option<wgpu::Buffer>,
    uv_buf: Option<wgpu::Buffer>,
    output_buf: Option<wgpu::Buffer>,
    staging_buf: Option<wgpu::Buffer>,
    /// Packed RGB10 HDR code values for the CPU-readback path.
    hdr_buf: Option<wgpu::Buffer>,
    hdr_staging_buf: Option<wgpu::Buffer>,
    /// Bound as the HDR output on SDR frames, which never write it. A
    /// binding has to exist; a real frame-sized buffer would be waste.
    hdr_dummy_buf: wgpu::Buffer,
    /// The same for the linear output, on the u8 route.
    linear_dummy_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    cached_y_size: u64,
    cached_uv_size: u64,
    cached_output_size: u64,
    cached_hdr_size: u64,
    /// The GPU-resident outputs handed to callers, kept to be handed out
    /// again once no caller holds them, as the NV12 converter's are (#2107).
    resident_rgba: crate::nv12_gpu::ResidentPool,
    resident_codes: crate::nv12_gpu::ResidentPool,
}

/// What the GPU-resident conversion produced.
pub struct P010GpuOutput {
    /// sRGB-u8 RGBA. For SDR the grade's input; for HDR a tone-mapped
    /// display preview.
    pub rgba: std::sync::Arc<wgpu::Buffer>,
    /// Packed RGB10 PQ/HLG code values in the source primaries — present
    /// exactly when the source is HDR, and then the grade's input.
    pub hdr_codes: Option<std::sync::Arc<wgpu::Buffer>>,
}

/// Transfers that take the scene-referred route: PQ, HLG, and every camera
/// log curve (ids 100+). Their frames carry code values for the grade.
pub(crate) fn is_hdr_transfer(transfer_id: u32) -> bool {
    transfer_id == klipp_core_transfer::PQ
        || transfer_id == klipp_core_transfer::HLG
        || transfer_id >= klipp_core_transfer::FIRST_CAMERA_LOG
}

/// `klipp_core::color_ids` values, mirrored: klipp-effects sits below
/// klipp-core. `color_ids_are_in_step` below pins them.
mod klipp_core_transfer {
    pub const PQ: u32 = 3;
    pub const HLG: u32 = 4;
    pub const FIRST_CAMERA_LOG: u32 = 100;
}

/// `color_range` / `matrix_id` / `transfer_id` / `primaries_id`
/// come from `klipp_core::color_ids::ids(SourceColorspace)`. The
/// legacy `is_hdr` boolean is dropped — `transfer_id == TRANSFER_PQ`
/// is the new HDR signal, and `transfer_id == TRANSFER_HLG` similarly
/// triggers the tone-map. The shader's HDR tone-map is folded into
/// `display_to_linear` (PQ EOTF outputs scene-linear up to 10000
/// cd/m²; the existing SDR display transform handles the further
/// roll-off).
///
/// `primaries_matrix` carries the pre-computed source-primaries →
/// working-space matrix as 3 rows × `vec4<f32>` (12 floats; the 4th
/// component of each row is padding for WGSL's `vec3<f32>` alignment).
/// The engine builds it via
/// `klipp_color::camera_log::primaries_to_working_matrix(primaries,
/// working_colorspace)` and packs it through
/// `crate::color_wgsl::pack_primaries_matrix`. Sliding the working-
/// space choice into the matrix keeps the shader simple — one matmul
/// per pixel regardless of which working space the project is in.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct P010Params {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    y_stride: u32,    // bytes per Y row
    uv_stride: u32,   // bytes per UV row
    rotation: u32,
    color_range: u32,
    matrix_id: u32,
    transfer_id: u32,
    primaries_id: u32,
    write_linear: u32,
    primaries_matrix: [[f32; 4]; 3],
}

/// P010-specific WGSL — runs after the shared
/// [`crate::color_wgsl::WGSL_PRELUDE`] + [`crate::color_wgsl::WGSL_HELPERS`].
/// Reads 10-bit packed Y/UV from P010 (semi-planar, u16 LE), uses
/// the shared colourspace dispatch, and adds a simple ACES filmic
/// tone-map for HDR (PQ / HLG) sources so the SDR viewer doesn't
/// see clipped highlights.
const SHADER_P010_BODY: &str = include_str!(concat!(env!("OUT_DIR"), "/shaders/decode/p010_body.wgsl"));

fn p010_shader_source() -> String {
    let mut s = String::with_capacity(
        crate::color_wgsl::WGSL_PRELUDE.len()
            + crate::color_wgsl::WGSL_HELPERS.len()
            + SHADER_P010_BODY.len()
            + crate::nv12_gpu::RESIZE_WGSL.len()
            + 256,
    );
    s.push_str(crate::color_wgsl::WGSL_PRELUDE);
    s.push_str(crate::color_wgsl::WGSL_HELPERS);
    // The `linear` output is a frame in the chain's own format, so its
    // store goes through `pack`, as the NV12 decoder's does.
    s.push_str(crate::gpu::shader_pixel_storage_source());
    s.push_str(crate::nv12_gpu::RESIZE_WGSL);
    s.push_str(SHADER_P010_BODY);
    s
}

/// [`p010_shader_source`], built once, so the pipeline and
/// `gpu::shader_sources`' checks read the same text.
pub(crate) fn p010_shader() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(p010_shader_source)
}

impl GpuP010Converter {
    /// How many GPU-resident output buffers the converter keeps to hand
    /// out again (#2107). For tests.
    #[doc(hidden)]
    pub fn resident_buffers_kept(&self) -> usize {
        self.resident_rgba.kept()
    }

    /// Create a new GPU P010 converter. Shares the process-wide
    /// `GpuContext` — see `GpuNv12Converter::new` for the rationale.
    pub fn new() -> Option<Self> {
        let ctx = crate::gpu_context::GpuContext::shared()?;
        Self::with_context(ctx)
    }

    /// Create a P010 converter that reuses an existing `GpuContext`.
    /// Enables GPU-resident decode → grade chain handoff in phase 4.
    pub fn with_context(ctx: &crate::gpu_context::GpuContext) -> Option<Self> {
        let device = ctx.device.clone();
        let queue = ctx.queue.clone();

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("p010_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    }, count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    }, count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false, min_binding_size: None,
                    }, count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false, min_binding_size: None,
                    }, count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false, min_binding_size: None,
                    }, count: None,
                },
                // Linear working-space f32, for the render's route.
                wgpu::BindGroupLayoutEntry {
                    binding: 5, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false, min_binding_size: None,
                    }, count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("p010_pl"), bind_group_layouts: &[&bind_group_layout], push_constant_ranges: &[],
        });

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("p010_to_rgba"),
            source: wgpu::ShaderSource::Wgsl(p010_shader().into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("p010_to_rgba"),
            layout: Some(&pipeline_layout),
            module: &shader_module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            crate::shader_check::shader_failed("P010 converter", &err.to_string());
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("p010_params"),
            size: std::mem::size_of::<P010Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let hdr_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("p010_hdr_dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // A *separate* dummy for the linear slot. One buffer bound twice
        // as writable storage in a single bind group is an aliasing
        // violation, which wgpu reports by failing the dispatch.
        let linear_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("p010_linear_dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        info!("P010 GPU converter initialized");

        Some(Self {
            device,
            queue,
            pipeline, bind_group_layout,
            y_buf: None, uv_buf: None, output_buf: None, staging_buf: None,
            hdr_buf: None, hdr_staging_buf: None, hdr_dummy_buf, linear_dummy_buf,
            params_buf,
            cached_y_size: 0, cached_uv_size: 0, cached_output_size: 0, cached_hdr_size: 0,
            resident_rgba: crate::nv12_gpu::ResidentPool::new("p010_out_gpu_resident"),
            resident_codes: crate::nv12_gpu::ResidentPool::new("p010_hdr_codes_gpu_resident"),
        })
    }

    /// Clone the Arc-wrapped wgpu device. Pair with [`Self::queue`]
    /// and the buffer returned from
    /// [`Self::convert_p010_to_rgba_on_gpu`] to assemble a
    /// `klipp_engine::GpuFrame`.
    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Clone the Arc-wrapped wgpu queue. See [`Self::device`].
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
        std::sync::Arc::clone(&self.queue)
    }

    fn ensure_buf(buf: &mut Option<wgpu::Buffer>, cached: &mut u64, size: u64, label: &str, usage: wgpu::BufferUsages, device: &wgpu::Device) {
        if size <= *cached { return; }
        let size = (size + 3) & !3;
        *buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label), size, usage, mapped_at_creation: false,
        }));
        *cached = size;
    }

    /// Convert one P010 frame to sRGB-u8 RGBA on the GPU.
    ///
    /// - `y_data`: Y plane bytes (`src_height` rows, each `y_stride` bytes).
    ///   Each pixel is a 16-bit LE sample with the value in upper 10 bits.
    /// - `uv_data`: interleaved UV (`src_height/2` rows × `uv_stride` bytes).
    ///   Each chroma pair is two 16-bit LE samples (U then V, upper-10).
    /// - `color_ids`: `[range, matrix_id, transfer_id, primaries_id]`
    ///   from `klipp_core::color_ids::ids(SourceColorspace)`. Drives
    ///   range normalisation, YUV→RGB matrix, EOTF, and HDR
    ///   tone-mapping. `primaries_id` is no longer consumed by the
    ///   shader — gamut conversion now goes through `primaries_matrix`.
    /// - `primaries_matrix`: source-primaries → working-space matrix
    ///   packed via [`crate::color_wgsl::pack_primaries_matrix`].
    #[allow(clippy::too_many_arguments)]
    pub fn convert_p010_to_rgba(
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

        Self::ensure_buf(&mut self.y_buf, &mut self.cached_y_size, y_size,
            "p010_y", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, &self.device);
        Self::ensure_buf(&mut self.uv_buf, &mut self.cached_uv_size, uv_size,
            "p010_uv", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, &self.device);
        Self::ensure_buf(&mut self.output_buf, &mut self.cached_output_size, output_size,
            "p010_out", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, &self.device);
        // Staging shares output_size; allocated lazily here so the
        // first call doesn't depend on a separate `ensure_*`.
        if self.staging_buf.is_none() || self.cached_output_size > 0 {
            // Recreate when output_size grew (Self::ensure_buf above
            // would have updated cached_output_size).
            let need_size = self.cached_output_size;
            let recreate = self.staging_buf.as_ref()
                .map(|b| b.size() < need_size).unwrap_or(true);
            if recreate {
                self.staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("p010_staging"),
                    size: need_size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
        }

        // HDR sources write their codes too; give them a real buffer so
        // nothing is written out of bounds. This path returns only the
        // u8 preview — `convert_p010_hdr_codes` is the one that reads the
        // codes back.
        let hdr = is_hdr_transfer(color_ids[2]);
        if hdr {
            let hdr_size = (dst_width as u64) * (dst_height as u64) * 4;
            Self::ensure_buf(&mut self.hdr_buf, &mut self.cached_hdr_size, hdr_size,
                "p010_hdr", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, &self.device);
        }

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();
        let output_buf = self.output_buf.as_ref().unwrap();
        let staging = self.staging_buf.as_ref().unwrap();
        let hdr_binding = if hdr { self.hdr_buf.as_ref().unwrap() } else { &self.hdr_dummy_buf };

        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        let params = P010Params {
            src_width, src_height, dst_width, dst_height,
            y_stride, uv_stride, rotation,
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 0,
            primaries_matrix,
        };
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: y_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: uv_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: hdr_binding.as_entire_binding() },
                // The linear output is the one going unwritten this time.
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.linear_dummy_buf.as_entire_binding(),
                },
            ],
        });

        let workgroups_x = dst_width.div_ceil(16);
        let workgroups_y = dst_height.div_ceil(16);

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }
        encoder.copy_buffer_to_buffer(output_buf, 0, staging, 0, output_size);
        self.queue.submit(std::iter::once(encoder.finish()));

        // Non-blocking poll loop: blocking Wait / WaitForSubmissionIndex
        // deadlock against the UI thread's continuous submits when
        // the device is shared. See gpu.rs::poll_until_mapped.
        let buffer_slice = staging.slice(..output_size);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| { let _ = tx.send(result); });
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::P010Convert);
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
                warn!("P010 GPU converter: readback failed, returning zeros");
                vec![0u8; output_size as usize]
            }
        }
    }

    /// GPU-resident variant of [`Self::convert_p010_to_rgba`]: runs
    /// the P010→RGBA compute shader (BT.2020+PQ tone-map when
    /// `is_hdr`, BT.709 otherwise) and returns the output
    /// `wgpu::Buffer` without downloading to CPU. Freshly allocated
    /// per call (flagged `STORAGE | COPY_SRC`) so the caller can
    /// retain the buffer across subsequent conversions and/or feed
    /// it into the grade chain as a compute-shader input in phase 4.
    ///
    /// Pair with [`Self::device`] / [`Self::queue`] to assemble a
    /// `klipp_engine::GpuFrame`.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_p010_to_rgba_on_gpu(
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
        self.convert_p010_on_gpu(
            y_data, uv_data, src_width, src_height, y_stride, uv_stride,
            dst_width, dst_height, rotation, color_ids, primaries_matrix,
        )
        .rgba
    }

    /// GPU-resident conversion returning every output: the u8 buffer
    /// and, for HDR sources, the packed RGB10 code values the grade reads.
    /// Both freshly allocated, so the caller owns them.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_p010_on_gpu(
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
    ) -> P010GpuOutput {
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;
        crate::gpu_memory::note_decoded_frame(dst_width, dst_height);

        Self::ensure_buf(
            &mut self.y_buf, &mut self.cached_y_size, y_size,
            "p010_y", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            &self.device,
        );
        Self::ensure_buf(
            &mut self.uv_buf, &mut self.cached_uv_size, uv_size,
            "p010_uv", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            &self.device,
        );

        // The caller's to hold: a buffer nothing else holds now, from the
        // pool, not the internal `output_buf` the readback paths share.
        let output_buf = self.resident_rgba.take(&self.device, output_size);

        let hdr_codes = is_hdr_transfer(color_ids[2]).then(|| self.resident_codes.take(&self.device, (dst_width as u64) * (dst_height as u64) * 4));
        let hdr_binding: &wgpu::Buffer = match hdr_codes.as_ref() {
            Some(b) => b,
            None => &self.hdr_dummy_buf,
        };

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();

        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        let params = P010Params {
            src_width, src_height, dst_width, dst_height,
            y_stride, uv_stride, rotation,
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 0,
            primaries_matrix,
        };
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: y_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: uv_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: hdr_binding.as_entire_binding() },
                // The linear output is the one going unwritten this time.
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.linear_dummy_buf.as_entire_binding(),
                },
            ],
        });

        let workgroups_x = dst_width.div_ceil(16);
        let workgroups_y = dst_height.div_ceil(16);

        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: None },
        );
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        P010GpuOutput { rgba: output_buf, hdr_codes }
    }

    /// Convert one HDR P010 frame and read back its packed RGB10 code
    /// values — the CPU path's equivalent of `hdr_codes`. Skips the u8
    /// readback entirely: for HDR the grade reads the codes, and the
    /// codes cost the same 4 bytes a pixel the u8 frame did.
    ///
    /// Returns `None` for a non-HDR transfer or a failed readback, so the
    /// caller can fall back to `convert_p010_to_rgba`.
    #[allow(clippy::too_many_arguments)]
    /// [`Self::convert_p010_on_gpu`] at full precision: the frame lands
    /// as linear working-space f32 rather than sRGB u8, ready to be a
    /// grade chain's input slot.
    ///
    /// This is the render's route, and the reason it exists is a
    /// measurement: a 10-bit frame that cannot take this path is scaled
    /// to RGBA64 by swscale instead, which costs **42 ms** a 4K frame.
    /// The shader computes the linear value either way — `write_linear`
    /// only decides which one is discarded — so the whole conversion,
    /// rotation and resize included, replaces that.
    ///
    /// The buffer comes back on **this converter's device**, so a caller
    /// meaning to hand it to the grade chain must have built the converter
    /// with [`Self::with_context`] on the same context the chain uses.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_p010_to_linear_on_gpu(
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
        Self::ensure_buf(
            &mut self.y_buf, &mut self.cached_y_size, y_data.len() as u64,
            "p010_y", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            &self.device,
        );
        Self::ensure_buf(
            &mut self.uv_buf, &mut self.cached_uv_size, uv_data.len() as u64,
            "p010_uv", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            &self.device,
        );

        // 16 bytes a pixel, and the caller takes ownership, so this is not
        // one of the internal caches.
        let linear_buf = std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("p010_linear_gpu_resident"),
            size: (dst_width as u64) * (dst_height as u64) * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        crate::gpu_memory::made(crate::gpu_memory::Made::ConvertToLinear, linear_buf.size());

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();
        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        let params = P010Params {
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
            label: Some("p010_to_linear_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: y_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: uv_buf.as_entire_binding() },
                // The u8 and code outputs are the ones going unwritten.
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.linear_dummy_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: self.hdr_dummy_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: linear_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("p010_to_linear") },
        );
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

    #[allow(clippy::too_many_arguments)]
    pub fn convert_p010_hdr_codes(
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
        if !is_hdr_transfer(color_ids[2]) {
            return None;
        }
        let out = self.convert_p010_on_gpu(
            y_data, uv_data, src_width, src_height, y_stride, uv_stride,
            dst_width, dst_height, rotation, color_ids, primaries_matrix,
        );
        let codes = out.hdr_codes?;
        let size = codes.size();
        if self.hdr_staging_buf.as_ref().is_none_or(|b| b.size() < size) {
            self.hdr_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("p010_hdr_staging"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let staging = self.hdr_staging_buf.as_ref().unwrap();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("p010_hdr_readback"),
        });
        encoder.copy_buffer_to_buffer(&codes, 0, staging, 0, size);
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = staging.slice(..size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::P010Convert);
        let mapped = crate::gpu::poll_until_mapped(&self.device, &rx);
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Idle);
        match mapped {
            Ok(Ok(())) => {
                let words = {
                    let view = slice.get_mapped_range();
                    bytemuck::cast_slice::<u8, u32>(&view).to_vec()
                };
                staging.unmap();
                Some(words)
            }
            _ => {
                warn!("P010 GPU converter: HDR code readback failed");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic 4×4 P010 frame: all pixels at code value 940
    /// (=10-bit Y "white" in limited range), gray chroma (512). After
    /// conversion in SDR mode this should be near-white sRGB.
    fn flat_p010(w: u32, h: u32, y10: u32, u10: u32, v10: u32) -> (Vec<u8>, Vec<u8>) {
        // P010 layout: each sample is 16-bit LE, value is in upper 10 bits.
        let pack = |v: u32| -> [u8; 2] {
            let x = ((v & 0x3FF) << 6) as u16;
            x.to_le_bytes()
        };
        let mut y_plane = Vec::with_capacity((w * h * 2) as usize);
        for _ in 0..(w * h) {
            y_plane.extend_from_slice(&pack(y10));
        }
        let mut uv_plane = Vec::with_capacity((w * h / 2) as usize);
        // UV plane: interleaved U, V at half resolution.
        for _ in 0..(w * h / 4) {
            uv_plane.extend_from_slice(&pack(u10));
            uv_plane.extend_from_slice(&pack(v10));
        }
        (y_plane, uv_plane)
    }

    #[test]
    fn p010_white_sdr_yields_near_white_rgba() {
        let Some(mut conv) = GpuP010Converter::new() else {
            eprintln!("skipping p010_white_sdr_yields_near_white_rgba: no GPU");
            return;
        };
        let (w, h) = (4u32, 4u32);
        let (y, uv) = flat_p010(w, h, 940, 512, 512); // limited-range white, neutral chroma
        let y_stride = w * 2;
        let uv_stride = w * 2; // U+V interleaved at half-h, full-w pairs
        // Limited BT.709 SDR — ID encoding from
        // klipp_core::color_ids: [LIMITED, BT709, BT709, BT709] = all 0u.
        let sdr_ids: [u32; 4] = [0, 0, 0, 0];
        let hdr_ids: [u32; 4] = [
            0, // limited range
            2, // MATRIX_BT2020_NCL
            3, // TRANSFER_PQ
            1, // PRIMARIES_BT2020
        ];
        let out = conv.convert_p010_to_rgba(
            &y, &uv, w, h, y_stride, uv_stride, w, h, 0,
            sdr_ids, crate::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED,
        );
        assert_eq!(out.len(), (w * h * 4) as usize);
        // Sample one pixel — should be near (255, 255, 255, 255).
        let r = out[0]; let g = out[1]; let b = out[2]; let a = out[3];
        assert!(r >= 245 && g >= 245 && b >= 245,
            "expected near-white RGBA, got ({r}, {g}, {b}, {a})");
        assert_eq!(a, 255);
        let _ = hdr_ids;
    }

    #[test]
    fn p010_black_sdr_yields_near_black_rgba() {
        let Some(mut conv) = GpuP010Converter::new() else { return };
        let (w, h) = (4u32, 4u32);
        let (y, uv) = flat_p010(w, h, 64, 512, 512); // limited-range black, neutral chroma
        let out = conv.convert_p010_to_rgba(
            &y, &uv, w, h, w * 2, w * 2, w, h, 0,
            [0, 0, 0, 0], crate::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED,
        );
        let r = out[0]; let g = out[1]; let b = out[2];
        assert!(r <= 4 && g <= 4 && b <= 4,
            "expected near-black RGBA, got ({r}, {g}, {b})");
    }

    #[test]
    fn p010_hdr_pq_white_tone_maps_to_visible_brightness() {
        let Some(mut conv) = GpuP010Converter::new() else { return };
        let (w, h) = (4u32, 4u32);
        // Limited-range PQ "diffuse white" reference is ~580/1023 in
        // 10-bit code (around 100 nits in PQ). Use that value to test
        // that tone-mapping doesn't crush diffuse white to either
        // black or fully-saturated white.
        let (y, uv) = flat_p010(w, h, 580, 512, 512);
        // [LIMITED, BT2020_NCL, PQ, BT2020] = [0, 2, 3, 1]
        let out = conv.convert_p010_to_rgba(
            &y, &uv, w, h, w * 2, w * 2, w, h, 0,
            [0, 2, 3, 1], crate::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED,
        );
        let r = out[0]; let g = out[1]; let b = out[2];
        // After Hable tone-map at exposure=4, 100 nits should land in
        // the upper midtones — somewhere in [80, 240]. Anything far
        // outside that range means the tone-mapping math is broken.
        for c in [r, g, b] {
            assert!((80..=240).contains(&c),
                "HDR PQ diffuse white tone-mapped channel = {c}, expected 80-240");
        }
        // Neutral chroma → channels should be approximately equal.
        let max_diff = [r, g, b].iter().max().unwrap() - [r, g, b].iter().min().unwrap();
        assert!(max_diff <= 5, "HDR neutral-chroma channels must be near-equal, got ({r}, {g}, {b})");
    }
}
