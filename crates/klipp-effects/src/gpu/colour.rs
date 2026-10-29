//! Colour on the GPU (#2890): the primary grade and its colour-page
//! companions (balance, HDR zones, wheels, curves, vibrance, the warper,
//! clarity, hue/sat, soft clip), 3D LUTs and colour space transforms, and the
//! colour effects (levels, gamma, HSV, invert, posterize, channel mixer,
//! equalize), with their params and shaders.

use super::*;

impl GpuEffectsPipeline {
    /// Apply per-clip color grading on the GPU. Returns the processed RGBA data.
    /// Accepts the pre-converted lift/gamma/gain RGB tuples from wheel_offset_to_rgb,
    /// and temperature / tint as a white-balance matrix
    /// (`klipp_color::white_balance::white_balance_matrix`; identity for none).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn apply_color_grade(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        exposure: f32,
        contrast: f32,
        // Already grade-encoded: `klipp_grade_model::encode_contrast_pivot`.
        contrast_pivot_encoded: f32,
        saturation: f32,
        white_balance: [[f32; 3]; 3],
        highlights: f32,
        shadows: f32,
        whites: f32,
        blacks: f32,
        lift: (f32, f32, f32),
        gamma: (f32, f32, f32),
        gain: (f32, f32, f32),
        offset: (f32, f32, f32),
        log_wheels: Option<[(f32, f32, f32); 3]>,
        wheel_shape: klipp_grade_model::WheelShape,
        hue: f32,
        global_mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuColorGradeParams {
            width,
            height,
            exposure,
            contrast,
            saturation,
            wb: flat3(white_balance),
            highlights,
            shadows,
            whites,
            blacks,
            lift_r: lift.0,
            lift_g: lift.1,
            lift_b: lift.2,
            gamma_r: gamma.0,
            gamma_g: gamma.1,
            gamma_b: gamma.2,
            gain_r: gain.0,
            gain_g: gain.1,
            gain_b: gain.2,
            offset_r: offset.0,
            offset_g: offset.1,
            offset_b: offset.2,
            hue,
            global_mix,
            contrast_pivot_encoded,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            encoding: self.grade_encoding,
            log_mode: log_wheels.is_some() as u32,
            log_wheels: flat_wheels(log_wheels),
            log_low_range: wheel_shape.low_range,
            log_high_range: wheel_shape.high_range,
            luma_mix: wheel_shape.luma_mix,
        });
        self.dispatch(&self.pipelines.color_grade_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply color balance (white balance, hue rotation, global mix) on GPU.
    /// `white_balance` is temperature / tint as a matrix on linear light
    /// (`klipp_color::white_balance::white_balance_matrix`).
    pub fn apply_color_balance_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        white_balance: [[f32; 3]; 3],
        hue: f32,
        global_mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuColorBalanceParams {
            width, height, wb: flat3(white_balance), hue, global_mix,
            _pad0: 0, _pad1: 0,
        });
        self.dispatch(&self.pipelines.color_balance_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply the HDR zones (#240) on the GPU, as
    /// [`crate::cpu_grade::hdr_zones`] does on the CPU.
    pub fn apply_hdr_zones_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, terms: &klipp_grade_model::hdr_zones::Terms) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuHdrZonesParams::new(width, height, terms));
        self.dispatch(&self.pipelines.hdr_zones_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply per-wheel saturation & brightness adjustments on GPU.
    pub fn apply_wheel_sat_bri_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        lift_sat: f32, lift_bri: f32,
        gamma_sat: f32, gamma_bri: f32,
        gain_sat: f32, gain_bri: f32,
        offset_sat: f32, offset_bri: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuWheelSatBriParams {
            width, height,
            lift_sat, lift_bri, gamma_sat, gamma_bri,
            gain_sat, gain_bri, offset_sat, offset_bri,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });
        self.dispatch(&self.pipelines.wheel_sat_bri_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply RGB curves on GPU using pre-computed LUTs.
    /// `lut_data` must be 4×256 f32 values (master, red, green, blue).
    pub fn apply_rgb_curves_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        lut_data: &[f32; 1024], // 4 × 256
        preserve_luma: bool,
        mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        // Upload curve LUT data
        self.upload(&self.rgb_curves_lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuRgbCurvesParams {
            width, height,
            preserve_luma: if preserve_luma { 1 } else { 0 },
            mix,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Create bind group with 4 bindings (reuse huesat_bgl layout)
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rgb_curves_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.rgb_curves_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.rgb_curves_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    pub fn apply_vibrance_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, amount: f32) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuVibranceParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        self.dispatch(&self.pipelines.vibrance_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply 12-sector color warper on GPU.
    pub fn apply_color_warper_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, hue_shifts: &[f32; 12], sat_shifts: &[f32; 12]) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuColorWarperParams {
            width, height, _pad0: 0, _pad1: 0,
            hue_shifts: *hue_shifts, sat_shifts: *sat_shifts,
        });
        self.dispatch(&self.pipelines.color_warper_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply clarity (midtone unsharp mask) on GPU using 3-pass approach.
    pub fn apply_clarity_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, amount: f32, radius: u32) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        let _luma_size = (width * height * 4) as u64; // store luma as u32 per pixel
        self.ensure_buffers(byte_size);
        // Need a third buffer for blur intermediate
        if self.buf_c.is_none() || self.current_buf_size < byte_size {
            self.buf_c = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("buf_c"), size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let radius = radius.max(1);

        // Pass 1: horizontal blur (input RGBA → buf_b luma)
        self.write_params(&GpuClarityBlurParams { width, height, radius, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        self.dispatch(&self.pipelines.clarity_blur_h_pipeline, buf_a, buf_b, workgroups);

        // Pass 2: vertical blur (buf_b luma → buf_c blurred luma)
        self.dispatch(&self.pipelines.clarity_blur_v_pipeline, buf_b, buf_c, workgroups);

        // Pass 3: apply unsharp mask (input RGBA + blurred luma → output RGBA)
        self.write_params(&GpuClarityApplyParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clarity_apply_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: buf_c.as_entire_binding() },
            ],
        });
        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.clarity_apply_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply hue/sat curves on the GPU using pre-computed LUTs.
    /// `lut_data` must be [`HUESAT_LUT_LEN`] f32 values — seven curves,
    /// 256 entries each.
    pub fn apply_huesat_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        mix: f32,
        lut_data: &[f32; HUESAT_LUT_LEN],
        active: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        // Upload LUT data
        self.upload(&self.huesat_lut_buf, 0, bytemuck::cast_slice(lut_data));

        // Write params
        self.write_params(&GpuHueSatParams {
            width,
            height,
            mix,
            active,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Create bind group with 4 bindings
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("huesat_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.huesat_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.huesat_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply a 3D LUT with trilinear interpolation on GPU.
    /// `lut_data` is RGB triples in B-major order, `lut_size` is the grid dimension (e.g. 33).
    /// `bracket` carries the LUT's primaries around the lookup.
    pub fn apply_3d_lut_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        lut_data: &[f32],
        lut_size: u32,
        opacity: f32,
        tetrahedral: bool,
        bracket: LutBracket,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        if opacity <= 0.0 {
            return linear_rgba.to_vec();
        }

        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        // (Re-)create LUT buffer if needed
        let lut_byte_size = (lut_data.len() * 4) as u64;
        if self.lut3d_buf.is_none() || self.lut3d_buf_size < lut_byte_size {
            self.lut3d_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lut3d_data"),
                size: lut_byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.lut3d_buf_size = lut_byte_size;
        }
        let lut_buf = self.lut3d_buf.as_ref().unwrap();
        self.upload(lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuLut3dParams::new(width, height, lut_size, opacity, tetrahedral, bracket));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut3d_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.lut3d_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply standalone soft-clip / hard-clip transform on GPU.
    /// Per-channel Reinhard-style rolloff above `threshold`. With
    /// `knee == 0` this becomes a hard clip at `threshold`; larger
    /// `knee` gives smoother highlights asymptotic to 1.0. Value
    /// transform — alpha is passed through; same pixel format and
    /// buffer layout as the input.
    pub fn apply_soft_clip_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        threshold: f32,
        knee: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuSoftClipParams {
            width,
            height,
            threshold,
            knee,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
            _pad3: 0,
        });

        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.dispatch(&self.pipelines.soft_clip_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Dispatch-only colour-grade pass. Reads `in_is_a`-slot, writes the other.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_color_grade_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        exposure: f32,
        contrast: f32,
        // Already grade-encoded: `klipp_grade_model::encode_contrast_pivot`.
        contrast_pivot_encoded: f32,
        saturation: f32,
        white_balance: [[f32; 3]; 3],
        highlights: f32,
        shadows: f32,
        whites: f32,
        blacks: f32,
        lift: (f32, f32, f32),
        gamma: (f32, f32, f32),
        gain: (f32, f32, f32),
        offset: (f32, f32, f32),
        log_wheels: Option<[(f32, f32, f32); 3]>,
        wheel_shape: klipp_grade_model::WheelShape,
        hue: f32,
        global_mix: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuColorGradeParams {
            width, height, exposure, contrast, saturation, wb: flat3(white_balance),
            highlights, shadows, whites, blacks,
            lift_r: lift.0, lift_g: lift.1, lift_b: lift.2,
            gamma_r: gamma.0, gamma_g: gamma.1, gamma_b: gamma.2,
            gain_r: gain.0, gain_g: gain.1, gain_b: gain.2,
            offset_r: offset.0, offset_g: offset.1, offset_b: offset.2,
            hue, global_mix, contrast_pivot_encoded,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            encoding: self.grade_encoding,
            log_mode: log_wheels.is_some() as u32,
            log_wheels: flat_wheels(log_wheels),
            log_low_range: wheel_shape.low_range,
            log_high_range: wheel_shape.high_range,
            luma_mix: wheel_shape.luma_mix,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        // Disjoint-field borrow: read from chain_bg_3binding while
        // taking &mut on chain_encoder. Pool must be pre-populated.
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.color_grade_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only colour-balance pass.
    pub fn apply_color_balance_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        white_balance: [[f32; 3]; 3],
        hue: f32,
        global_mix: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuColorBalanceParams {
            width, height, wb: flat3(white_balance), hue, global_mix,
            _pad0: 0, _pad1: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.color_balance_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only HDR zones pass (#240).
    pub fn apply_hdr_zones_chain(&mut self, in_is_a: bool, width: u32, height: u32, terms: &klipp_grade_model::hdr_zones::Terms) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuHdrZonesParams::new(width, height, terms));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.hdr_zones_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only wheel sat/bri pass.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_wheel_sat_bri_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        lift_sat: f32, lift_bri: f32,
        gamma_sat: f32, gamma_bri: f32,
        gain_sat: f32, gain_bri: f32,
        offset_sat: f32, offset_bri: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuWheelSatBriParams {
            width, height,
            lift_sat, lift_bri, gamma_sat, gamma_bri,
            gain_sat, gain_bri, offset_sat, offset_bri,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.wheel_sat_bri_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only RGB curves pass. Uploads LUT, writes params, dispatches.
    pub fn apply_rgb_curves_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        lut_data: &[f32; 1024],
        preserve_luma: bool,
        mix: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        self.upload(&self.rgb_curves_lut_buf, 0, bytemuck::cast_slice(lut_data));
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuRgbCurvesParams {
            width, height,
            preserve_luma: if preserve_luma { 1 } else { 0 },
            mix,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_rgb_curves[slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.rgb_curves_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only vibrance pass.
    pub fn apply_vibrance_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        amount: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuVibranceParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.vibrance_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only 12-sector colour warper pass.
    pub fn apply_color_warper_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        hue_shifts: &[f32; 12],
        sat_shifts: &[f32; 12],
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuColorWarperParams {
            width, height, _pad0: 0, _pad1: 0,
            hue_shifts: *hue_shifts, sat_shifts: *sat_shifts,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.color_warper_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only clarity (midtone unsharp-mask) pass. Uses `buf_c` as
    /// scratch for the horizontal/vertical luma blur. The caller must have
    /// already called `ensure_buffers` for a frame that fits in `buf_c` too.
    pub fn apply_clarity_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        amount: f32,
        radius: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let byte_size = (width * height * 16) as u64;
        // buf_c must exist and match the current size. Normally
        // `ensure_chain_buffers` has already built it at the current
        // size via `ensure_buffers`; this branch is defensive for the
        // case where clarity runs outside the chain (shouldn't happen).
        // If we do recreate buf_c, rebuild the clarity/3-binding pools
        // that reference it.
        if self.buf_c.is_none() || self.current_buf_size < byte_size {
            self.buf_c = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("buf_c"), size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.rebuild_chain_bind_group_pools();
        }
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let radius = radius.max(1);

        // Clarity is 3 passes (blur_h, blur_v, apply). Two distinct
        // params structs (`GpuClarityBlurParams` for the blurs,
        // `GpuClarityApplyParams` for the unsharp-mask combine) — two
        // ring slots are sufficient since the two blur passes share the
        // same params struct. We still claim two slots to keep each
        // logical params write in its own backing buffer; blur_h and
        // blur_v happen to hold identical values, so we could share,
        // but keeping them separate simplifies reasoning and leaves
        // room if the blur params ever diverge.
        let blur_slot = self.next_params_slot();
        self.write_chain_params(blur_slot, &GpuClarityBlurParams { width, height, radius, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        let apply_slot = self.next_params_slot();
        self.write_chain_params(apply_slot, &GpuClarityApplyParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });

        // Pass 1: horizontal blur (in RGBA → out luma scratch).
        // 3-binding pool AtoB/BtoA at `blur_slot`.
        {
            let bind_group = self.chain_bg_3binding[blur_slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
                .as_ref()
                .expect(Self::CHAIN_BG_EXPECT);
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            Self::record_compute_dispatch(enc, &self.pipelines.clarity_blur_h_pipeline, bind_group, workgroups);
        }

        // Pass 2: vertical blur (out luma → buf_c blurred luma).
        // The "input" here is what Pass 1 wrote to, which is buf_b when
        // in_is_a or buf_a when !in_is_a. That's (buf_b, buf_c) or
        // (buf_a, buf_c) respectively — AtoC/BtoC configs in the pool.
        // Note: `!in_is_a` inverts parity because the vertical pass
        // reads what the horizontal pass just wrote.
        {
            let bind_group = self.chain_bg_3binding[blur_slot * 4 + Self::chain_cfg_3b_ac(!in_is_a)]
                .as_ref()
                .expect(Self::CHAIN_BG_EXPECT);
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            Self::record_compute_dispatch(enc, &self.pipelines.clarity_blur_v_pipeline, bind_group, workgroups);
        }

        // Pass 3: unsharp mask (in RGBA + buf_c blurred luma → out RGBA).
        // 4-binding clarity_apply pool keyed on (in_is_a) × (buf_c as
        // binding 3).
        let bind_group = self.chain_bg_clarity_apply[apply_slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.clarity_apply_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only hue/sat curves pass. Uploads the 6×256 LUT, writes
    /// params, and dispatches.
    pub fn apply_huesat_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        mix: f32,
        lut_data: &[f32; HUESAT_LUT_LEN],
        active: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        self.upload(&self.huesat_lut_buf, 0, bytemuck::cast_slice(lut_data));
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuHueSatParams { width, height, mix, active });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_huesat[slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.huesat_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only 3D LUT pass with trilinear interpolation.
    pub fn apply_3d_lut_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        lut_data: &[f32],
        lut_size: u32,
        opacity: f32,
        tetrahedral: bool,
        bracket: LutBracket,
    ) {
        if self.is_device_lost() {
            return;
        }
        if opacity <= 0.0 {
            // No-op: copy in → out into the chain encoder. The batched
            // submit runs it alongside the surrounding dispatches.
            let byte_size = (width * height * 16) as u64;
            // Split the shared `self` borrow: encoder is on self, buf_a/buf_b
            // are on self. `copy_buffer_to_buffer` takes `&Buffer`, so we
            // need to hold the buffer refs separately from the &mut encoder.
            // wgpu::Buffer isn't Clone, but copy_buffer_to_buffer doesn't
            // retain them, so we can re-extract each borrow in sequence by
            // matching on the Options directly.
            let buf_a = self.buf_a.as_ref().expect("ensure_chain_buffers must be called first");
            let buf_b = self.buf_b.as_ref().expect("ensure_chain_buffers must be called first");
            let (input, output) = if in_is_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            enc.copy_buffer_to_buffer(input, 0, output, 0, byte_size);
            return;
        }
        let lut_byte_size = (lut_data.len() * 4) as u64;
        if self.lut3d_buf.is_none() || self.lut3d_buf_size < lut_byte_size {
            self.lut3d_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lut3d_data"),
                size: lut_byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.lut3d_buf_size = lut_byte_size;
        }
        // Claim the params slot first so subsequent `&self` borrows
        // (lut_buf, chain_in_out) don't clash with the `&mut self` that
        // `next_params_slot` needs.
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuLut3dParams::new(width, height, lut_size, opacity, tetrahedral, bracket));
        let lut_buf = self.lut3d_buf.as_ref().unwrap();
        self.upload(lut_buf, 0, bytemuck::cast_slice(lut_data));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let (input, output) = self.chain_in_out(in_is_a);
        // NOTE: lut3d_buf is dynamically sized (different LUT dimensions
        // produce different buffer sizes), so it's not pooled — a fresh
        // bind group is created per dispatch. This path runs at most
        // once per chain (≤1 3D LUT node per grade) so the cost is bounded.
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut3d_chain_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lut_buf.as_entire_binding() },
            ],
        });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.lut3d_pipeline, &bind_group, workgroups);
    }

    /// Equalize's histogram and CDF of `linear` (straight RGBA, `width` ×
    /// `height`) as the chain builds them on the GPU (#1215), read back:
    /// for the tests that hold them to `crate::equalize_cdf`. `None` on a
    /// lost device.
    #[doc(hidden)]
    pub fn equalize_histogram_and_cdf(&mut self, linear: &[f32], width: u32, height: u32) -> Option<([u32; EQUALIZE_CDF_LEN], [f32; EQUALIZE_CDF_LEN])> {
        if self.is_device_lost() {
            return None;
        }
        self.ensure_recording_buffers(width, height);
        let frame = self.buf_a.as_ref()?;
        self.upload(frame, 0, bytemuck::cast_slice(linear));
        self.write_params(&EqualizeParams { width, height, strength: 1.0, _pad0: 0, luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0] });
        let (hist, cdf) = (&self.equalize_hist_buf, &self.equalize_cdf_buf);
        let bind = |a: &wgpu::Buffer, b: &wgpu::Buffer, c: &wgpu::Buffer| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("equalize_probe"),
                layout: &self.pipelines.huesat_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: a.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: b.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: c.as_entire_binding() },
                ],
            })
        };
        let mut encoder = self.encoder(None);
        encoder.clear_buffer(hist, 0, None);
        Self::record_compute_dispatch(&mut encoder, &self.pipelines.equalize_histogram_pipeline, &bind(frame, hist, cdf), (width * height).div_ceil(WORKGROUP_SIZE));
        Self::record_compute_dispatch(&mut encoder, &self.pipelines.equalize_cdf_pipeline, &bind(hist, cdf, hist), 1);
        self.submit(encoder);
        let counts: Vec<u32> = bytemuck::cast_slice(&self.download(hist, EQUALIZE_CDF_LEN as u64 * 4)).to_vec();
        let ramp: Vec<f32> = bytemuck::cast_slice(&self.download(cdf, EQUALIZE_CDF_LEN as u64 * 4)).to_vec();
        Some((counts.try_into().ok()?, ramp.try_into().ok()?))
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuColorGradeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) exposure: f32,
    pub(super) contrast: f32,
    pub(super) saturation: f32,
    /// Temperature / tint as a white-balance matrix, row-major
    /// (`klipp_color::white_balance`).
    pub(super) wb: [f32; 9],
    pub(super) highlights: f32,
    pub(super) shadows: f32,
    pub(super) whites: f32,
    pub(super) blacks: f32,
    pub(super) lift_r: f32,
    pub(super) lift_g: f32,
    pub(super) lift_b: f32,
    pub(super) gamma_r: f32,
    pub(super) gamma_g: f32,
    pub(super) gamma_b: f32,
    pub(super) gain_r: f32,
    pub(super) gain_g: f32,
    pub(super) gain_b: f32,
    pub(super) offset_r: f32,
    pub(super) offset_g: f32,
    pub(super) offset_b: f32,
    pub(super) hue: f32,
    pub(super) global_mix: f32,
    /// Where contrast pivots, already grade-encoded.
    ///
    /// Sits in what was `_pad0`, so the uniform's size and every field
    /// offset after it are unchanged. Encoded on this side rather than
    /// in the shader: one `powf` per frame instead of one per sample,
    /// and the CPU twin is handed the identical number rather than
    /// deriving its own, which is a way parity drifts.
    pub(super) contrast_pivot_encoded: f32,
    /// Working-space luma coefficients used by the saturation /
    /// tone-controls / lift-gamma-gain stages of `SHADER_COLOR_GRADE`.
    /// `(0.2126, 0.7152, 0.0722)` for Rec709Linear,
    /// `(0.2722, 0.6741, 0.0537)` for AcesCG (AP1), etc. Populated by
    /// the engine via `GpuEffectsPipeline::set_luma_coefs`.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    /// The grade encoding, `klipp_grade_model::Encoding::as_u8` (#467).
    pub(super) encoding: u32,
    /// Non-zero: log-mode wheels (`log_wheels` = low, mid, high RGB)
    /// replace lift/gamma/gain/offset.
    pub(super) log_mode: u32,
    pub(super) log_wheels: [f32; 9],
    /// The log wheels' low and high band centres, and the wheels' luma
    /// mix (#900): `klipp_grade_model::WheelShape`.
    pub(super) log_low_range: f32,
    pub(super) log_high_range: f32,
    pub(super) luma_mix: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuHueSatParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mix: f32,
    /// Bit `c` set: curve `c` was edited and applies.
    pub(super) active: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuColorBalanceParams {
    pub(super) width: u32,
    pub(super) height: u32,
    /// Temperature / tint as a white-balance matrix, row-major.
    pub(super) wb: [f32; 9],
    pub(super) hue: f32,
    pub(super) global_mix: f32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
}

/// `SHADER_HDR_ZONES`'s uniform: each zone's gain in stops per channel
/// and its saturation, then its boundary and falloff. 256 bytes, the
/// whole params slot.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuHdrZonesParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
    pub(super) luma: [f32; 4],
    pub(super) gain_sat: [[f32; 4]; klipp_grade_model::hdr_zones::ZONES],
    pub(super) edge: [[f32; 4]; klipp_grade_model::hdr_zones::ZONES],
}

impl GpuHdrZonesParams {
    fn new(width: u32, height: u32, t: &klipp_grade_model::hdr_zones::Terms) -> Self {
        let mut gain_sat = [[0.0; 4]; klipp_grade_model::hdr_zones::ZONES];
        let mut edge = [[0.0; 4]; klipp_grade_model::hdr_zones::ZONES];
        for i in 0..klipp_grade_model::hdr_zones::ZONES {
            let g = t.gain_stops[i];
            gain_sat[i] = [g[0], g[1], g[2], t.saturation[i]];
            edge[i] = [t.boundary[i], t.falloff[i], 0.0, 0.0];
        }
        Self { width, height, _pad0: 0, _pad1: 0, luma: [t.luma[0], t.luma[1], t.luma[2], 0.0], gain_sat, edge }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuWheelSatBriParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) lift_sat: f32,
    pub(super) lift_bri: f32,
    pub(super) gamma_sat: f32,
    pub(super) gamma_bri: f32,
    pub(super) gain_sat: f32,
    pub(super) gain_bri: f32,
    pub(super) offset_sat: f32,
    pub(super) offset_bri: f32,
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuRgbCurvesParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) preserve_luma: u32,
    pub(super) mix: f32,
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuVibranceParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) amount: f32, pub(super) _pad: u32,
    pub(super) luma_yr: f32, pub(super) luma_yg: f32, pub(super) luma_yb: f32, pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuColorWarperParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) _pad0: u32, pub(super) _pad1: u32,
    pub(super) hue_shifts: [f32; 12],
    pub(super) sat_shifts: [f32; 12],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuClarityBlurParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) radius: u32, pub(super) _pad: u32,
    pub(super) luma_yr: f32, pub(super) luma_yg: f32, pub(super) luma_yb: f32, pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuClarityApplyParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) amount: f32, pub(super) _pad: u32,
    pub(super) luma_yr: f32, pub(super) luma_yg: f32, pub(super) luma_yb: f32, pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuGradeMixParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mix: f32,
    pub(super) _pad: u32,
}

// Colour grade — the big one. Exposure, tone controls, LGG+offset
// wheels, contrast, saturation, temperature/tint, global mix. All
// run in linear so BT.709 luma matches its coefficient domain,
// exposure `2^stops` produces real photographic stops, contrast
// pivots at 0.18 (perceptual mid-grey), tone-control band centres
// map to the linear equivalents of the old 0.5/0.8/0.2 sRGB
// thresholds (0.214/0.604/0.033 linear — close perceptually, but
// now physically correct).
pub(super) const SHADER_COLOR_GRADE: &str = gpu_shader!("colour/color_grade.wgsl");

// Hue/Sat curves — six HSL-based curves (hue-vs-hue, hue-vs-sat,
// hue-vs-lum, lum-vs-sat, sat-vs-sat, orange-vs-sat). HSL math is
// colour-space-agnostic (works on any normalised RGB), but feeding
// it linear-light values means hue/sat/lum values shift vs sRGB
// (grey lum = 0.18 linear ≈ 0.46 sRGB). Curve authors re-tune in
// linear; the physical luminance axis of lum-vs-sat is now correct.
pub(super) const SHADER_HUESAT: &str = gpu_shader!("colour/huesat.wgsl");

// ─── Soft Clip GPU Shader ───────────────────────────────────────────────────
//
// Per-channel Reinhard-style highlight rolloff. Mirrors the CPU
// curve in klipp-color/soft_clip.rs::soft_clip_value: passthrough
// below `threshold`, hard clip when `knee == 0`, asymptotic-to-1.0
// rolloff when `knee > 0`. Value transform — alpha is preserved.
pub(super) const SHADER_SOFT_CLIP: &str = gpu_shader!("colour/soft_clip.wgsl");

// LUT3D is defined in sRGB 0-1 space (each LUT entry maps an sRGB
// triplet to an sRGB triplet). Storage is native linear-f32, so we
// encode pixel → sRGB, sample the LUT, then decode the result back to
// linear. The opacity blend is done in linear space.
//
// A LUT tagged with other primaries than the working space's is
// bracketed: `pre` takes the pixel into the LUT's input primaries
// before the encode, `post` takes the decoded result from its output
// primaries back, as `klipp_effects::apply_3d_lut_bracketed` does on
// the CPU (#422). `bracket` bit 0 turns `pre` on, bit 1 `post`.
pub(super) const SHADER_LUT3D: &str = gpu_shader!("colour/lut3d.wgsl");

// Invert — 1 − x on the display-encoded picture: the photo negative
// editors mean by invert. (Inverting linear light turned 18% grey into
// a near-white, not a negative.)
pub(super) const SHADER_INVERT: &str = gpu_shader!("colour/invert.wgsl");

// Posterize — quantise to N levels, even on the display scale (in linear
// light the shadows collapsed into one band). White stays white.
pub(super) const SHADER_POSTERIZE: &str = gpu_shader!("colour/posterize.wgsl");

// The colour-space transform effect (#1746), as
// `colour_transform::cst_pixel` computes it: the input curve's table is
// the slot table's first 256 floats, the output curve's the next 256.
pub(super) const SHADER_CST: &str = gpu_shader!("colour/cst.wgsl");

// Channel mixer — 3x3 linear combination of RGB channels. Linear
// mixing corresponds to real light-combining operations (e.g.
// "synthesise luma from 0.7 R + 0.2 G + 0.1 B"). The sRGB version
// mixed gamma-encoded bytes, which silently under- or over-weighted
// depending on channel intensity.
pub(super) const SHADER_CHANNEL_MIXER: &str = gpu_shader!("colour/channel_mixer.wgsl");

// Gamma — power curve on the display-encoded picture, as in other
// editors.
pub(super) const SHADER_GAMMA: &str = gpu_shader!("colour/gamma.wgsl");

// HSV adjust — hue shift + saturation + value multiplier, on the
// display-encoded picture: HSV is a perceptual model, and in linear
// light its saturation and value misread every midtone.
pub(super) const SHADER_HSV_ADJUST: &str = gpu_shader!("colour/hsv_adjust.wgsl");

// Levels — black/white point remap + gamma on the display-encoded
// picture: the points are on the scale the user sees, as in other
// editors (in linear light, "0.1" meant sRGB 89).
pub(super) const SHADER_LEVELS: &str = gpu_shader!("colour/levels.wgsl");

// Equalize's histogram (#1215), as `crate::equalize_cdf` counts it: each
// pixel's display luma, `yw · encode(rgb)` summed in that order and
// clamped to 0–1, in bin `floor(l × 255)`. Each workgroup counts into its
// own bins, then adds them into the frame's; integer atomics, so the
// counts don't depend on order.
pub(super) const SHADER_EQUALIZE_HISTOGRAM: &str = gpu_shader!("colour/equalize_histogram.wgsl");

// Equalize's CDF from its histogram (#1215), `crate::equalize_cdf`'s
// arithmetic in its order: from the first occupied bin, the share of the
// pixels at or below each bin past it, or the identity ramp for a frame of
// one bin. 256 bins: one thread walks them.
pub(super) const SHADER_EQUALIZE_CDF: &str = gpu_shader!("colour/equalize_cdf.wgsl");

// Histogram equalisation — flatten the distribution of display-encoded
// luma, the brightness the user sees. The CDF is the histogram passes'
// (or `crate::equalize_cdf`'s, on the break path).
pub(super) const SHADER_EQUALIZE: &str = gpu_shader!("colour/equalize.wgsl");

// Colour balance — temperature / tint as a white-balance matrix, an
// optional HSL hue rotation (the Corrector's), and a mix with the input.
pub(super) const SHADER_COLOR_BALANCE: &str = gpu_shader!("colour/color_balance.wgsl");

// HDR zones (#240): `klipp_grade_model::hdr_zones::apply` per pixel.
// Zone 0 is Global, 1 to 3 reach down from their boundary, 4 to 6 up.
pub(super) const SHADER_HDR_ZONES: &str = gpu_shader!("colour/hdr_zones.wgsl");

// Wheel sat/bri — per-wheel saturation and brightness adjustments,
// band-weighted. Same formula as the sRGB version but in linear so
// BT.709 luma sits in its defined domain and the wheel-band curves
// (lift = 1 − luma, gain = luma, gamma = sin curve) target physical
// tonal regions.
pub(super) const SHADER_WHEEL_SAT_BRI: &str = gpu_shader!("colour/wheel_sat_bri.wgsl");

// RGB curves — four LUT-lookup curves (master + per-channel), on the
// display-encoded picture: the curve's axis is the sRGB encoding of the
// light, so mid-grey sits at 0.46 where a colourist expects it, not at
// 0.18. Past the curve's ends it continues straight at the slope it
// ends with, so an identity curve is identity on an HDR highlight
// instead of flattening it to white, and a curve the user flattened at
// the top still flattens. Negative channels (a wide-gamut colour in a
// narrower working space) take the curve mirrored, so they survive too.
// Preserve-luma and the mix stay in linear light, where luminance is.
pub(super) const SHADER_RGB_CURVES: &str = gpu_shader!("colour/rgb_curves.wgsl");

// Vibrance — saturation-weighted boost. Muted colours get a bigger
// push; already-saturated colours (including skin) are protected.
// BT.709 luma now sits in its defined domain.
pub(super) const SHADER_VIBRANCE: &str = gpu_shader!("colour/vibrance.wgsl");

// Colour warper — 12-sector hue/sat shifting with cosine
// interpolation between sectors. HSL operates on linear RGB; hue
// values are colour-space-agnostic but lum in HSL is linear
// rather than sRGB-coded (grey = 0.18).
pub(super) const SHADER_COLOR_WARPER: &str = gpu_shader!("colour/color_warper.wgsl");

// Clarity — three-pass midtone unsharp mask. CLARITY_BLUR_H reads
// linear RGBA, computes linear-light BT.709 luma, writes the luma
// into the `.x` channel of a shared vec4<f32> intermediate buffer
// (other channels unused — we never downconvert or pack). CLARITY_BLUR_V
// averages that luma (still in `.x`). CLARITY_APPLY adds back detail
// weighted by linear midtone presence, centred on 0.18 linear.
pub(super) const SHADER_CLARITY_BLUR_H: &str = gpu_shader!("colour/clarity_blur_h.wgsl");

pub(super) const SHADER_CLARITY_BLUR_V: &str = gpu_shader!("colour/clarity_blur_v.wgsl");

pub(super) const SHADER_GRADE_MIX: &str = gpu_shader!("colour/grade_mix.wgsl");

pub(super) const SHADER_CLARITY_APPLY: &str = gpu_shader!("colour/clarity_apply.wgsl");
