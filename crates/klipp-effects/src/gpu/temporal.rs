//! Time on the GPU (#2890): temporal denoise and its neighbour frames,
//! deflicker, rolling-shutter and stabilisation corrections, and the old
//! film and VHS looks, with their params and shaders.

use super::*;

impl GpuEffectsPipeline {
    /// A neighbour frame for [`Self::apply_temporal_denoise_chain`], from
    /// linear working-space pixels on the CPU: a buffer of its own, never
    /// the chain's or the stack's, which the caller holds (an `Arc`) until
    /// the chain that reads it is submitted. `None` without a device or
    /// when the buffer can't be made.
    pub fn upload_neighbour(&self, linear: &[f32]) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if self.is_device_lost() || linear.is_empty() {
            return None;
        }
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_neighbour"),
            size: std::mem::size_of_val(linear) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.upload_frame(&buf, bytemuck::cast_slice(linear));
        Some(std::sync::Arc::new(buf))
    }

    /// [`Self::neighbour_from_u8`] from bytes of another size, `src_w` ×
    /// `src_h`, fitted into `width` × `height` as the chain's seed fits a
    /// frame shown (#2854): a viewer stream decoding below the render's
    /// size.
    pub fn neighbour_fit_from_u8(&self, input_u8: &wgpu::Buffer, src_w: u32, src_h: u32, width: u32, height: u32) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if (src_w, src_h) == (width, height) {
            return self.neighbour_from_u8(input_u8, width, height);
        }
        let pixels = u64::from(width) * u64::from(height);
        if self.is_device_lost() || pixels == 0 || src_w == 0 || src_h == 0 || input_u8.size() < u64::from(src_w) * u64::from(src_h) * 4 {
            return None;
        }
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neighbour_fit_from_u8"),
            size: pixels * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let (dst_w, dst_h, x_off, y_off) = fit_rect(src_w, src_h, width, height);
        let params = wgpu::util::DeviceExt::create_buffer_init(self.device.as_ref(), &wgpu::util::BufferInitDescriptor {
            label: Some("neighbour_fit_from_u8_params"),
            contents: bytemuck::bytes_of(&BridgeU8FitParams { src_w, src_h, out_w: width, out_h: height, dst_w, dst_h, x_off, y_off }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neighbour_fit_from_u8_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.pipelines.srgb8_lut.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("neighbour_fit_from_u8") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.bridge_u8_fit_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (pixels as u32).div_ceil(WORKGROUP_SIZE));
        }
        self.queue.submit(Some(encoder.finish()));
        Some(std::sync::Arc::new(out))
    }

    /// A neighbour frame the GPU holds as sRGB bytes (a viewer stream's
    /// decoded frame), decoded to linear into a buffer of its own for a
    /// temporal denoise to bind (#1988): nothing read back or uploaded.
    /// Never the chain's buffers or its parameters: the pass has a uniform
    /// of its own, and is submitted at once, ahead of the chain that reads
    /// it. `None` on a lost device or a buffer too small.
    pub fn neighbour_from_u8(&self, input_u8: &wgpu::Buffer, width: u32, height: u32) -> Option<std::sync::Arc<wgpu::Buffer>> {
        let pixels = u64::from(width) * u64::from(height);
        if self.is_device_lost() || pixels == 0 || input_u8.size() < pixels * 4 {
            return None;
        }
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_neighbour_from_u8"),
            size: pixels * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_neighbour_from_u8_params"),
            size: std::mem::size_of::<BridgeU8Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&params, 0, bytemuck::bytes_of(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 }));
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("temporal_neighbour_from_u8_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.pipelines.srgb8_lut.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("temporal_neighbour_from_u8") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.bridge_u8_to_linear_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (pixels as u32).div_ceil(WORKGROUP_SIZE));
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        Some(std::sync::Arc::new(out))
    }

    /// Dispatch-only temporal noise reduction (#1988): the chain's picture
    /// (in `buf_a` when `in_is_a`) against `neighbours` (none to two, each
    /// a frame of the picture's size), written to the other slot, as
    /// `temporal_denoise::denoise_frame` draws it. `false`, nothing
    /// recorded, without a chain, with more than two neighbours or one of
    /// the wrong size; the caller takes the CPU route then. `luma` is the
    /// guard's brightness weights, the working space's
    /// (`klipp_color::linear::luma_coefs`), as the CPU takes them (#2019).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_temporal_denoise_chain(&mut self, in_is_a: bool, width: u32, height: u32, neighbours: &[&wgpu::Buffer], threshold: f32, strength: f32, luma: [f32; 3]) -> bool {
        let bytes = u64::from(width) * u64::from(height) * 16;
        if self.is_device_lost() || self.chain_encoder.is_none() || neighbours.len() > 2 || neighbours.iter().any(|n| n.size() < bytes) {
            return false;
        }
        if self.buf_a.is_none() || self.buf_b.is_none() {
            return false;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuTemporalDenoiseParams {
            width,
            height,
            count: neighbours.len() as u32,
            _pad0: 0,
            denom: crate::temporal_denoise::denominator(threshold),
            strength: strength.clamp(0.0, 1.0),
            _pad1: 0,
            _pad2: 0,
            luma: [luma[0], luma[1], luma[2], 0.0],
        });
        let (input, output) = match (self.buf_a.as_ref(), self.buf_b.as_ref()) {
            (Some(a), Some(b)) if in_is_a => (a, b),
            (Some(a), Some(b)) => (b, a),
            _ => return false,
        };
        // An unread slot still needs a buffer bound: the input, read only.
        let before = neighbours.first().copied().unwrap_or(input);
        let after = neighbours.get(1).copied().unwrap_or(input);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_temporal_denoise"),
            layout: &self.pipelines.temporal_denoise_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: before.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: after.as_entire_binding() },
            ],
        });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.temporal_denoise_pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
        true
    }

    /// Temporal noise reduction of `input` (linear RGBA, `width` ×
    /// `height`) against `neighbours`, off the chain (#2266): the chain's
    /// kernel and parameters, so a frame is denoised to the same picture
    /// whether the chain draws it or this does. For a node whose output
    /// must be pixels (a parked frame holds it): the result read back,
    /// and the buffer it is in, for the holder to seed later chains from.
    /// Its own buffers and uniform, never the chain's. `None` on a lost
    /// device, a size that doesn't fit, more than two neighbours, or a
    /// failed read; the caller draws it on the CPU then.
    #[allow(clippy::too_many_arguments)]
    pub fn temporal_denoise_pixels(&mut self, input: &[f32], width: u32, height: u32, neighbours: &[&wgpu::Buffer], threshold: f32, strength: f32, luma: [f32; 3]) -> Option<(Vec<f32>, std::sync::Arc<wgpu::Buffer>)> {
        let pixels = (width as usize) * (height as usize);
        let bytes = (pixels * 16) as u64;
        if self.is_device_lost() || pixels == 0 || input.len() != pixels * 4 || neighbours.len() > 2 || neighbours.iter().any(|n| n.size() < bytes) {
            return None;
        }
        // The read-back's staging buffer, at the frame's size.
        self.ensure_buffers(bytes);
        let storage = |label| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let (inp, out) = (storage("temporal_denoise_in"), storage("temporal_denoise_out"));
        self.queue.write_buffer(&inp, 0, bytemuck::cast_slice(input));
        let params = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_denoise_params"),
            size: std::mem::size_of::<GpuTemporalDenoiseParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&params, 0, bytemuck::bytes_of(&GpuTemporalDenoiseParams {
            width,
            height,
            count: neighbours.len() as u32,
            _pad0: 0,
            denom: crate::temporal_denoise::denominator(threshold),
            strength: strength.clamp(0.0, 1.0),
            _pad1: 0,
            _pad2: 0,
            luma: [luma[0], luma[1], luma[2], 0.0],
        }));
        let before = neighbours.first().copied().unwrap_or(&inp);
        let after = neighbours.get(1).copied().unwrap_or(&inp);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("temporal_denoise_pixels_bg"),
            layout: &self.pipelines.temporal_denoise_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: inp.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: before.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: after.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("temporal_denoise_pixels") });
        Self::record_compute_dispatch(&mut encoder, &self.pipelines.temporal_denoise_pipeline, &bind_group, (pixels as u32).div_ceil(WORKGROUP_SIZE));
        self.queue.submit(std::iter::once(encoder.finish()));
        let read = self.with_downloaded(&out, bytes, |b| mapped_bytes_to_f32(b, pixels * 4)).filter(|v| v.len() == pixels * 4)?;
        Some((read, std::sync::Arc::new(out)))
    }

    pub fn upload_rsc_row_transforms(&mut self, row_transforms: &[crate::rolling_shutter::RowTransform]) {
        let byte_size = std::mem::size_of_val(row_transforms) as u64;
        if self.rsc_row_buf.as_ref().is_none_or(|_| self.rsc_row_buf_size < byte_size) {
            self.rsc_row_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rsc_row_transforms"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.rsc_row_buf_size = byte_size;
        }
        self.upload(self.rsc_row_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(row_transforms));
    }

    pub fn has_rsc_data(&self) -> bool {
        self.rsc_row_buf.is_some() && self.rsc_row_buf_size > 0
    }

    pub fn upload_stabilization_homography(&mut self, h: &[f32; 12]) {
        let byte_size = 48u64; // 12 x f32
        if self.stabilization_buf.is_none() {
            self.stabilization_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stabilization_homography"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.stabilization_buf_size = byte_size;
        }
        self.upload(self.stabilization_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(h));
    }

    /// Forget the stabilisation correction, so the effect passes the
    /// frame through until another is uploaded: a clip without analysis
    /// must not be warped by the last clip's (#698).
    pub fn clear_stabilization_homography(&mut self) {
        self.stabilization_buf = None;
    }

    /// Forget the rolling-shutter row transforms, as
    /// [`Self::clear_stabilization_homography`] does.
    pub fn clear_rsc_row_transforms(&mut self) {
        self.rsc_row_buf = None;
        self.rsc_row_buf_size = 0;
    }

    pub fn has_stabilization_data(&self) -> bool {
        self.stabilization_buf.is_some()
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct VhsParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tracking: f32,
    pub(super) bleed: f32,
    pub(super) noise: f32,
    pub(super) scanlines: f32,
    pub(super) seed: u32,
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct DeflickerParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) gain: f32, pub(super) strength: f32,
}

/// Temporal noise reduction (#1988): `temporal_denoise::denoise_frame`
/// for one pixel, line for line. The current pixel weighs 1; each
/// neighbour weighs a gaussian on its luma's difference from the
/// current one's; the weighted average is mixed back by `strength`.
/// Alpha is the current pixel's.
pub(super) const SHADER_TEMPORAL_DENOISE: &str = gpu_shader!("temporal/temporal_denoise.wgsl");

// Old film — gate weave, flicker, sepia, scratches, dust and a soft
// vignette, one pass, all driven by `seed` (the frame number).
pub(super) const SHADER_OLD_FILM: &str = gpu_shader!("temporal/old_film.wgsl");

// VHS — per-row tracking jitter with a wandering tear band, chroma
// bleed (luma from here, chroma from a few pixels left), tape noise
// and scanlines.
pub(super) const SHADER_VHS: &str = gpu_shader!("temporal/vhs.wgsl");

// Rolling shutter correction — per-row affine warp with bilinear
// sampling. Bilinear averaging benefits from linear light; without
// it, the four-tap interpolation darkens soft sub-pixel edges
// (the classic gamma-resize issue).
pub(super) const SHADER_ROLLING_SHUTTER: &str = gpu_shader!("temporal/rolling_shutter.wgsl");

// Stabilisation — per-frame homography warp with bilinear sampling.
// Bilinear averaging in linear preserves soft edge luminance through
// the warp.
pub(super) const SHADER_STABILIZATION: &str = gpu_shader!("temporal/stabilization.wgsl");

pub(super) const SHADER_DEFLICKER: &str = gpu_shader!("temporal/deflicker.wgsl");
