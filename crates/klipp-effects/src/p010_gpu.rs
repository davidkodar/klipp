//! GPU P010 (10-bit semi-planar YUV) → sRGB-u8 RGBA conversion.
//!
//! Replaces the CPU `sws_scale` path that costs ~21 ms per 4K frame
//! AND mishandles HDR (decoded PQ samples as if they were sRGB,
//! decoded BT.2020 chroma as if it were BT.709). On a 4K HDR clip the
//! CPU path produced ~10.5 fps with garbage colors. This GPU shader
//! lands the correct image in ~5-8 ms total (sub-ms compute + 33 MB
//! readback).
//!
//! Layered transformations performed in the shader, per pixel:
//!   1. Decode P010: 10-bit Y from upper bits of u16, similarly UV.
//!   2. Limited-range YUV → matrix RGB (BT.2020 for HDR, BT.709 for SDR-10).
//!   3. Apply EOTF: PQ for HDR, BT.1886/sRGB for SDR — pixel is now
//!      linear-light cd/m².
//!   4. (HDR only) Hable filmic tone-map to bring 10000-nit range
//!      into ~100-nit display range.
//!   5. (HDR only) Gamut-convert BT.2020 → BT.709 primaries.
//!   6. sRGB OETF + clamp to byte boundary.
//!
//! The shader chooses `is_hdr` from a uniform set by the host based
//! on the source frame's colorspace metadata. SDR 10-bit (most "high
//! quality" deliverables) uses the simpler path; HDR (PQ / Dolby
//! Vision) goes through the full chain.

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
    params_buf: wgpu::Buffer,
    cached_y_size: u64,
    cached_uv_size: u64,
    cached_output_size: u64,
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
    _pad1: u32,
    primaries_matrix: [[f32; 4]; 3],
}

/// P010-specific WGSL — runs after the shared
/// [`crate::color_wgsl::WGSL_PRELUDE`] + [`crate::color_wgsl::WGSL_HELPERS`].
/// Reads 10-bit packed Y/UV from P010 (semi-planar, u16 LE), uses
/// the shared colourspace dispatch, and adds a simple ACES filmic
/// tone-map for HDR (PQ / HLG) sources so the SDR viewer doesn't
/// see clipped highlights.
const SHADER_P010_BODY: &str = r#"
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
    color_range: u32,
    matrix_id: u32,
    transfer_id: u32,
    primaries_id: u32,
    _pad1: u32,
    primaries_row0: vec4<f32>,
    primaries_row1: vec4<f32>,
    primaries_row2: vec4<f32>,
}

// 16-bit LE sample at byte offset `byte_idx` from u32-packed arrays.
fn load_u16_le_y(byte_idx: u32) -> u32 {
    let word_idx = byte_idx / 4u;
    let byte_off = byte_idx % 4u;
    let w = y_plane[word_idx];
    if (byte_off <= 2u) {
        return (w >> (byte_off * 8u)) & 0xFFFFu;
    } else {
        let lo = (w >> 24u) & 0xFFu;
        let hi = y_plane[word_idx + 1u] & 0xFFu;
        return lo | (hi << 8u);
    }
}

fn load_u16_le_uv(byte_idx: u32) -> u32 {
    let word_idx = byte_idx / 4u;
    let byte_off = byte_idx % 4u;
    let w = uv_plane[word_idx];
    if (byte_off <= 2u) {
        return (w >> (byte_off * 8u)) & 0xFFFFu;
    } else {
        let lo = (w >> 24u) & 0xFFu;
        let hi = uv_plane[word_idx + 1u] & 0xFFu;
        return lo | (hi << 8u);
    }
}

// HDR → SDR tone map. Input is linear cd/m² normalised so 1.0 =
// 10000 nits (the PQ peak). Output is linear in [0, 1] where 1.0
// is "SDR display peak". Scale 100-nit HDR diffuse white to 0.18
// linear (Rec.709 mid-grey), then push through ACES filmic.
fn aces_filmic_scalar(x: f32) -> f32 {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), 0.0, 1.0);
}

fn tone_map_hdr_vec(rgb: vec3<f32>) -> vec3<f32> {
    let exposed = rgb * 18.0;
    return vec3<f32>(
        aces_filmic_scalar(max(exposed.x, 0.0)),
        aces_filmic_scalar(max(exposed.y, 0.0)),
        aces_filmic_scalar(max(exposed.z, 0.0)),
    );
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = gid.y;
    if (dx >= params.dst_width || dy >= params.dst_height) { return; }

    // Source coord (rotation + nearest-neighbour resize).
    var sx: u32;
    var sy: u32;
    if (params.rotation == 0u) {
        sx = dx * params.src_width / params.dst_width;
        sy = dy * params.src_height / params.dst_height;
    } else if (params.rotation == 90u) {
        sx = dy * params.src_width / params.dst_height;
        sy = (params.dst_width - 1u - dx) * params.src_height / params.dst_width;
    } else if (params.rotation == 180u) {
        sx = (params.dst_width - 1u - dx) * params.src_width / params.dst_width;
        sy = (params.dst_height - 1u - dy) * params.src_height / params.dst_height;
    } else { // 270
        sx = (params.dst_height - 1u - dy) * params.src_width / params.dst_height;
        sy = dx * params.src_height / params.dst_width;
    }
    sx = min(sx, params.src_width - 1u);
    sy = min(sy, params.src_height - 1u);

    // P010 layout: Y plane u16/pixel (10 valid bits in upper bits),
    // UV plane interleaved u16 pairs at half resolution. Strides
    // are in bytes.
    let y_byte_idx = sy * params.y_stride + sx * 2u;
    let y_word16 = load_u16_le_y(y_byte_idx);
    // Upper 10 bits hold the sample; lower 6 are padding zeros.
    let y_10 = f32(y_word16 >> 6u);

    let uv_x = sx / 2u;
    let uv_y = sy / 2u;
    let u_byte_idx = uv_y * params.uv_stride + uv_x * 4u;
    let v_byte_idx = u_byte_idx + 2u;
    let u_word16 = load_u16_le_uv(u_byte_idx);
    let v_word16 = load_u16_le_uv(v_byte_idx);
    let u_10 = f32(u_word16 >> 6u);
    let v_10 = f32(v_word16 >> 6u);

    // Five-stage colourspace pipeline (HDR adds the tone-map step):
    //   1. Range-aware normalise (limited 64/940 vs full 0/1023).
    //   2. YUV → display-encoded RGB via the source matrix.
    //   3. Inverse EOTF → linear (in source primaries; cd/m² for PQ).
    //   4. (HDR only) ACES filmic tone-map scene-linear → SDR linear.
    //   5. Source primaries → working space via the uniform matrix.
    // Then re-encode to sRGB-u8 for the existing CPU/GPU hand-off.
    let yuv = yuv_normalise(y_10, u_10, v_10, params.color_range, 1023.0);
    let rgb_display = yuv_to_rgb_display(yuv, params.matrix_id);
    var rgb_linear  = display_to_linear(rgb_display, params.transfer_id);
    if (params.transfer_id == TRANSFER_PQ || params.transfer_id == TRANSFER_HLG) {
        rgb_linear = tone_map_hdr_vec(rgb_linear);
    }
    let rgb_working = vec3<f32>(
        dot(params.primaries_row0.xyz, rgb_linear),
        dot(params.primaries_row1.xyz, rgb_linear),
        dot(params.primaries_row2.xyz, rgb_linear),
    );
    let rgb_srgb  = linear_to_srgb_vec(rgb_working);

    let r_b = u32(clamp(rgb_srgb.x * 255.0 + 0.5, 0.0, 255.0));
    let g_b = u32(clamp(rgb_srgb.y * 255.0 + 0.5, 0.0, 255.0));
    let b_b = u32(clamp(rgb_srgb.z * 255.0 + 0.5, 0.0, 255.0));
    let pixel = r_b | (g_b << 8u) | (b_b << 16u) | (255u << 24u);
    output[dy * params.dst_width + dx] = pixel;
}
"#;

fn p010_shader_source() -> String {
    let mut s = String::with_capacity(
        crate::color_wgsl::WGSL_PRELUDE.len()
            + crate::color_wgsl::WGSL_HELPERS.len()
            + SHADER_P010_BODY.len(),
    );
    s.push_str(crate::color_wgsl::WGSL_PRELUDE);
    s.push_str(crate::color_wgsl::WGSL_HELPERS);
    s.push_str(SHADER_P010_BODY);
    s
}

impl GpuP010Converter {
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
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("p010_pl"), bind_group_layouts: &[&bind_group_layout], push_constant_ranges: &[],
        });

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("p010_to_rgba"),
            source: wgpu::ShaderSource::Wgsl(p010_shader_source().into()),
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
            warn!("P010 GPU converter: shader compilation failed: {err}");
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("p010_params"),
            size: std::mem::size_of::<P010Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        info!("P010 GPU converter initialized");

        Some(Self {
            device,
            queue,
            pipeline, bind_group_layout,
            y_buf: None, uv_buf: None, output_buf: None, staging_buf: None,
            params_buf,
            cached_y_size: 0, cached_uv_size: 0, cached_output_size: 0,
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

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();
        let output_buf = self.output_buf.as_ref().unwrap();
        let staging = self.staging_buf.as_ref().unwrap();

        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        let params = P010Params {
            src_width, src_height, dst_width, dst_height,
            y_stride, uv_stride, rotation,
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            _pad1: 0,
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
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;

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

        // Fresh output buffer per call — we're handing ownership to
        // the caller, so the internal `output_buf` cache isn't safe
        // to reuse here.
        let output_buf = std::sync::Arc::new(self.device.create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("p010_out_gpu_resident"),
                size: output_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        ));

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
            _pad1: 0,
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

        output_buf
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
            assert!(c >= 80 && c <= 240,
                "HDR PQ diffuse white tone-mapped channel = {c}, expected 80-240");
        }
        // Neutral chroma → channels should be approximately equal.
        let max_diff = [r, g, b].iter().max().unwrap() - [r, g, b].iter().min().unwrap();
        assert!(max_diff <= 5, "HDR neutral-chroma channels must be near-equal, got ({r}, {g}, {b})");
    }
}
