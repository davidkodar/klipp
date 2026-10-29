//! Keys, mattes and masks on the GPU (#2890): the chroma, luma and
//! difference keys and the qualifier, masks drawn into mattes, a matte
//! applied, cut out or kept in a slot, and the alpha passes that shape one,
//! with their params and shaders.

use super::*;

impl GpuEffectsPipeline {
    /// Generate a mask matte on GPU. Supports an unbounded number
    /// of masks of any shape (circle / rectangle / linear gradient
    /// / bezier polygon) — per-mask shape data lands in a storage
    /// buffer rather than the previous fixed-size uniform, and
    /// pooled bezier control points live in a second storage
    /// buffer keyed by per-record `bezier_offset` / `bezier_count`.
    ///
    /// Returns greyscale RGBA8 (R=G=B=matte, A=255).
    pub fn generate_mask_gpu(
        &mut self,
        width: u32,
        height: u32,
        masks: &[GpuMaskDef],
    ) -> Vec<u8> {
        if self.is_device_lost() {
            let _ = masks;
            return vec![0u8; (width * height * 4) as usize];
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Pre-allocate everything `&mut self` needs BEFORE we
        // start handing out `&` borrows of self.buf_b — wgpu::Buffer
        // is not Clone in 22.x so we can't make an owned copy to
        // hand to a `&mut self` dispatch. Instead we make
        // `dispatch_mask_v2` `&self`-only and build buffers up
        // front.
        let (records, points) = Self::build_mask_storage(masks);
        let records_bytes = (records.len() * std::mem::size_of::<MaskRecord>()) as u64;
        let points_bytes = (points.len() * std::mem::size_of::<[f32; 2]>()) as u64;
        self.ensure_mask_storage_buffers(records_bytes, points_bytes);

        let dest = self.buf_b.as_ref().expect("ensure_buffers populated buf_b");
        self.dispatch_mask_v2(width, height, &records, &points, masks.len() as u32, dest, workgroups);
        self.download_raw_rgba_as_u8(dest, (width * height) as usize)
    }

    /// Build the per-mask records and pooled bezier-points arrays
    /// from a slice of [`GpuMaskDef`]. Returns the records (one
    /// per input mask, with `bezier_offset` / `bezier_count` set
    /// for bezier shapes) and the flat point pool (concatenation
    /// of every bezier mask's points in stack order).
    pub(super) fn build_mask_storage(masks: &[GpuMaskDef]) -> (Vec<MaskRecord>, Vec<[f32; 2]>) {
        let mut records: Vec<MaskRecord> = Vec::with_capacity(masks.len());
        let mut points: Vec<[f32; 2]> = Vec::new();
        for m in masks {
            let (offset, count) = if m.shape_type == 3 {
                let off = points.len() as u32;
                let n = m.bezier_points.len() as u32;
                points.extend(m.bezier_points.iter().map(|&(x, y)| [x, y]));
                (off, n)
            } else {
                (0, 0)
            };
            records.push(MaskRecord {
                shape_type: m.shape_type,
                flags: m.flags,
                bezier_offset: offset,
                bezier_count: count,
                cx: m.cx, cy: m.cy, p1: m.p1, p2: m.p2,
                p3: m.p3, feather: m.feather, feather_inner: m.feather_inner, _pad1: 0,
                w0: [m.warp[0], m.warp[1], m.warp[2], 0.0],
                w1: [m.warp[3], m.warp[4], m.warp[5], 0.0],
                w2: [m.warp[6], m.warp[7], m.warp[8], 0.0],
            });
        }
        (records, points)
    }

    /// Ensure the records + points storage buffers are at least
    /// the requested sizes. Storage buffers can't be size 0 in
    /// wgpu (it'd fail validation), so we floor each at 16 bytes
    /// of placeholder data — empty-bezier-stack draws still need
    /// valid bind-group entries.
    pub(super) fn ensure_mask_storage_buffers(&mut self, records_bytes: u64, points_bytes: u64) {
        let records_bytes = records_bytes.max(96);  // one MaskRecord
        let points_bytes = points_bytes.max(8);     // one vec2<f32>
        let need_records = self.mask_records_buf.as_ref()
            .is_none_or(|b| b.size() < records_bytes);
        if need_records {
            self.mask_records_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mask_records_buf"),
                size: records_bytes.next_power_of_two().max(96),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let need_points = self.mask_points_buf.as_ref()
            .is_none_or(|b| b.size() < points_bytes);
        if need_points {
            self.mask_points_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mask_points_buf"),
                size: points_bytes.next_power_of_two().max(8),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
    }

    /// Write the storage buffers, build a 4-binding bind group on
    /// the v2 layout, and dispatch the mask pipeline writing to
    /// `dest` (which is `buf_b` for the CPU-download path and
    /// `matte_buf` for the GPU-resident chain path). Shared
    /// helper so the two callers stay in sync.
    ///
    /// Caller is responsible for `ensure_buffers` (the dest buffer)
    /// AND `ensure_mask_storage_buffers` (records + points buffers)
    /// BEFORE calling — those need `&mut self`, this dispatch only
    /// needs `&self` so we can hold a `&Buffer` to `dest` from
    /// inside `self.buf_b` / `self.matte_buf` while calling.
    pub(super) fn dispatch_mask_v2(
        &self,
        width: u32,
        height: u32,
        records: &[MaskRecord],
        points: &[[f32; 2]],
        mask_count: u32,
        dest: &wgpu::Buffer,
        workgroups: u32,
    ) {
        // Empty-stack guard: write a placeholder all-zero record
        // and point so the bind-group's storage buffers are
        // non-empty (wgpu validation rejects 0-byte storage
        // buffers in a binding). The shader's `mask_count` gate
        // means the placeholders never execute.
        let placeholder_record = [MaskRecord {
            shape_type: 0, flags: 0, bezier_offset: 0, bezier_count: 0,
            cx: 0.0, cy: 0.0, p1: 0.0, p2: 0.0,
            p3: 0.0, feather: 0.0, feather_inner: 0.0, _pad1: 0,
            w0: [0.0; 4], w1: [0.0; 4], w2: [0.0; 4],
        }];
        let placeholder_point = [[0.0_f32, 0.0_f32]];
        let records_bytes_to_write: &[u8] = if records.is_empty() {
            bytemuck::cast_slice(&placeholder_record)
        } else {
            bytemuck::cast_slice(records)
        };
        let points_bytes_to_write: &[u8] = if points.is_empty() {
            bytemuck::cast_slice(&placeholder_point)
        } else {
            bytemuck::cast_slice(points)
        };

        let records_buf = self.mask_records_buf.as_ref()
            .expect("ensure_mask_storage_buffers must run before dispatch_mask_v2");
        let points_buf = self.mask_points_buf.as_ref()
            .expect("ensure_mask_storage_buffers must run before dispatch_mask_v2");
        self.upload(records_buf, 0, records_bytes_to_write);
        self.upload(points_buf, 0, points_bytes_to_write);

        // write_params is &mut self elsewhere; inline the small
        // uniform write here so this helper stays &self.
        let params = GpuMaskParams {
            width, height,
            mask_count,
            _pad: 0,
        };
        self.write_params(&params);

        // Pull the bind group layout straight off the pipeline so
        // we don't have to keep a second handle around in the
        // pipeline struct for every dispatch.
        let bgl = self.pipelines.mask_pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask_v2_bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: dest.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: records_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: points_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.encoder(Some("mask_v2_enc"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mask_v2_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.mask_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        self.device.poll(wgpu::Maintain::Poll);
    }

    /// Apply qualifier (secondary color correction) on GPU.
    /// Generates HSL matte and applies secondary grade in a single pass.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_qualifier_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        hue_center: f32, hue_width: f32, hue_soft: f32,
        sat_low: f32, sat_high: f32, sat_soft: f32,
        lum_low: f32, lum_high: f32, lum_soft: f32,
        invert: bool,
        sec_exposure: f32, sec_saturation: f32, sec_hue_shift: f32, sec_temperature: f32,
        matte_preview: bool,
        rgb: Option<[f32; 7]>,
        volume: Option<QualifierVolume>,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        let (mode, ranges, v) = GpuQualifierParams::shape(rgb, volume);
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuQualifierParams {
            width,
            height,
            hue_center: hue_center / 360.0,
            hue_width: hue_width / 360.0,
            hue_soft: hue_soft / 360.0,
            sat_low,
            sat_high,
            sat_soft,
            lum_low,
            lum_high,
            lum_soft,
            invert: if invert { 1 } else { 0 },
            sec_exposure,
            sec_saturation,
            sec_hue_shift: sec_hue_shift.to_radians(),
            sec_temperature: sec_temperature / 100.0 * 0.1,
            matte_preview: if matte_preview { 1 } else { 0 },
            mode,
            _pad1: 0,
            _pad2: 0,
            rgb: ranges,
            _pad3: 0.0,
            keep_mean: v[0],
            keep_inv: [v[1], v[2], v[3]],
            remove_mean: v[4],
            remove_inv: [v[5], v[6], v[7]],
            reach: v[8],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.dispatch(&self.pipelines.qualifier_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply standalone luminance-threshold key on GPU. Mirrors
    /// [`Self::apply_qualifier_gpu`] but evaluates Rec.709 luma
    /// alone — no HSL, no secondary grade. Output is a greyscale
    /// matte (R=G=B=matte, A copied from input alpha) ready for
    /// downstream `MaskCutout` / `CombineMattes` / `ApplyMatte`
    /// consumption.
    pub fn apply_luma_key_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        low: f32,
        high: f32,
        soft: f32,
        invert: bool,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuLumaKeyParams {
            width,
            height,
            low,
            high,
            soft,
            invert: if invert { 1 } else { 0 },
            into_alpha: 0,
            _pad1: 0,
            luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
        });

        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.dispatch(&self.pipelines.luma_key_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Rasterize a mask matte into the dedicated `matte_buf` (instead
    /// of buf_b → CPU download as `generate_mask_gpu` does). Used by
    /// the GPU-resident `Source → Corrector → MaskCutout` chain so the
    /// matte never leaves the GPU. Caller is responsible for matching
    /// the matte_buf's lifetime to the chain's submit window — i.e.
    /// `matte_buf` write happens BEFORE `apply_mask_cutout_chain` in
    /// the same `begin_chain` / `submit_chain` pair.
    ///
    /// All shape variants are supported, including bezier polygons
    /// (per-mask shape data is in a storage buffer, not the
    /// uniform — there is no shape cap).
    pub fn generate_mask_gpu_to_matte_buf(
        &mut self,
        width: u32,
        height: u32,
        masks: &[GpuMaskDef],
    ) {
        if self.is_device_lost() {
            return;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let (records, points) = Self::build_mask_storage(masks);
        let records_bytes = (records.len() * std::mem::size_of::<MaskRecord>()) as u64;
        let points_bytes = (points.len() * std::mem::size_of::<[f32; 2]>()) as u64;
        self.ensure_mask_storage_buffers(records_bytes, points_bytes);

        let matte = self.matte_buf.as_ref().expect("matte_buf populated by ensure_buffers");
        self.dispatch_mask_v2(width, height, &records, &points, masks.len() as u32, matte, workgroups);
    }

    /// Dispatch-only mask cutout — multiplies the chain frame by
    /// `matte_buf`'s R channel and forces alpha=1.0. Caller must have
    /// written the matte to `matte_buf` (e.g. via
    /// `generate_mask_gpu_to_matte_buf`) before calling this. Standard
    /// chain conventions: reads from in_is_a slot, writes to opposite.
    pub fn apply_mask_cutout_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuMaskCutoutParams { width, height, _pad0: 0, _pad1: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_mask_cutout[slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.mask_cutout_pipeline, bind_group, workgroups);
    }

    /// Upload the difference key's clean plate, already resized to the
    /// frame and in linear light, unless the same one is already there.
    ///
    /// `key` identifies the plate to the *host*: its resize-cache
    /// generation plus the frame size. Contents are not compared, since
    /// reading 33 MB to decide whether to write 33 MB is the cost this
    /// avoids. A host that reuses a generation for different pixels
    /// would show a stale plate, so the executor derives it from the
    /// plate path and dimensions it already keys its own cache on.
    pub fn upload_difference_key_plate(
        &mut self,
        plate_linear: &[f32],
        key: (u64, u32, u32),
    ) {
        if self.is_device_lost() {
            return;
        }
        // Another plate while a key recorded in the open chain still reads
        // this one (#1266): the chain goes to the queue first and goes on
        // in a new encoder, as when its slots run out. The upload then
        // lands after that submit and before the next.
        if self.plate_key != Some(key) && self.plate_read_in_chain && self.chain_encoder.is_some() {
            self.submit_chain();
            self.begin_chain();
        }
        let bytes = std::mem::size_of_val(plate_linear) as u64;
        let needs_alloc = self.plate_buf.as_ref().is_none_or(|b| b.size() < bytes);
        if needs_alloc {
            self.plate_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("difference_key_plate"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            // A fresh buffer holds nothing, so whatever was keyed before
            // is no longer in it.
            self.plate_key = None;
        }
        if self.plate_key == Some(key) {
            return;
        }
        if let Some(buf) = self.plate_buf.as_ref() {
            self.upload_frame(buf, bytemuck::cast_slice(plate_linear));
            self.plate_key = Some(key);
        }
    }

    /// Whether a plate matching `key` is already resident, so the
    /// executor can skip building the resized plate at all.
    pub fn has_difference_key_plate(&self, key: (u64, u32, u32)) -> bool {
        self.plate_buf.is_some() && self.plate_key == Some(key)
    }

    /// Dispatch-only ApplyMatte: blend the chain's picture (graded) with
    /// `kept` (ungraded), through `matte` (M3 step 17b). Standard chain
    /// conventions — reads the `in_is_a` slot, writes the opposite.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_matte_chain(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32, channel: crate::mask_mix::MaskChannel, invert: bool, kept: &KeptPicture, matte: &KeyedMatte) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        let [luma_yr, luma_yg, luma_yb] = self.luma_coefs;
        self.write_chain_params(slot, &GpuApplyMatteParams { width, height, mix, channel: channel as u32, luma_yr, luma_yg, luma_yb, invert: invert as u32 });
        let (a, b) = (
            self.buf_a.as_ref().expect(Self::CHAIN_BG_EXPECT),
            self.buf_b.as_ref().expect(Self::CHAIN_BG_EXPECT),
        );
        let (input, output) = if in_is_a { (a, b) } else { (b, a) };
        let (kept, matte) = (self.slot_pool.buffer(kept.0), self.slot_pool.buffer(matte.0));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_apply_matte"),
            layout: &self.pipelines.apply_matte_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: kept.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: matte.as_entire_binding() },
            ],
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.apply_matte_pipeline, &bind_group, workgroups);
    }

    /// Multiply layer `i`'s alpha by `factor` (a transition side's
    /// opacity, a missing matte source).
    pub fn stack_scale_alpha(&mut self, i: usize, factor: f32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        self.write_params(&ScaleAlphaParams { width, height, factor, _pad: 0 });
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        self.stack_pass(&self.pipelines.scale_alpha_pipeline, self.matte_buf.as_ref().unwrap(), &layer, wg, "stack_scale_alpha");
        true
    }

    /// Mask layer `consumer`'s alpha by a matte taken from layer
    /// `source`: `channel` (0 alpha, 1 luminance, 2 red, 3 green, 4
    /// blue), inverted or not, choked by `choke_px` (a square minimum),
    /// feathered by `feather_px` (the same box-Gaussian the CPU uses).
    #[allow(clippy::too_many_arguments)]
    pub fn stack_matte(&mut self, consumer: usize, source: usize, channel: u32, invert: bool, feather_px: f32, choke_px: f32) -> bool {
        if self.is_device_lost() || consumer >= self.stack_layers.len() || source >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let luma = self.luma_coefs;
        let source_buf = std::sync::Arc::clone(&self.stack_layers[source]);
        let consumer_buf = std::sync::Arc::clone(&self.stack_layers[consumer]);
        self.write_params(&MatteExtractParams { width, height, channel, invert: invert as u32, luma_r: luma[0], luma_g: luma[1], luma_b: luma[2], _pad: 0 });
        self.stack_pass(&self.pipelines.matte_extract_pipeline, &source_buf, self.matte_buf.as_ref().unwrap(), wg, "stack_matte_extract");
        if choke_px >= 0.5 {
            let radius = (choke_px.round() as u32).max(1);
            self.write_params(&MaskPoolParams { width, height, radius, _pad: 0 });
            self.stack_pass(&self.pipelines.mask_min_h_pipeline, self.matte_buf.as_ref().unwrap(), self.buf_c.as_ref().unwrap(), wg, "stack_mask_min_h");
            self.write_params(&MaskPoolParams { width, height, radius, _pad: 0 });
            self.stack_pass(&self.pipelines.mask_min_v_pipeline, self.buf_c.as_ref().unwrap(), self.matte_buf.as_ref().unwrap(), wg, "stack_mask_min_v");
        }
        if feather_px >= 0.25 {
            for r in crate::blur::boxes_for_gaussian(feather_px) {
                self.write_params(&BlurParams { width, height, radius: r as i32, _pad: 0 });
                self.stack_pass(&self.pipelines.blur_h_pipeline, self.matte_buf.as_ref().unwrap(), self.buf_c.as_ref().unwrap(), wg, "stack_feather_h");
                self.write_params(&BlurParams { width, height, radius: r as i32, _pad: 0 });
                self.stack_pass(&self.pipelines.blur_v_pipeline, self.buf_c.as_ref().unwrap(), self.matte_buf.as_ref().unwrap(), wg, "stack_feather_v");
            }
        }
        self.write_params(&ScaleAlphaParams { width, height, factor: 1.0, _pad: 0 });
        self.stack_pass(&self.pipelines.matte_apply_pipeline, self.matte_buf.as_ref().unwrap(), &consumer_buf, wg, "stack_matte_apply");
        true
    }

    /// Draw `key`'s matte from the chain's picture (in `buf_a` when
    /// `in_is_a`) into a pool slot of its own, recorded in the chain's
    /// encoder: the picture isn't touched, and nothing is read back.
    /// `None` when there is no chain, the pool's budget refuses, or a
    /// difference key has no plate; the caller keys the CPU way then.
    pub fn key_into_matte_slot(&mut self, key: &GpuKey, in_is_a: bool, width: u32, height: u32) -> Option<KeyedMatte> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        if matches!(key, GpuKey::DifferenceKey { .. }) && self.plate_buf.is_none() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Matte }, |k| slot_buffer(device, k)).ok()?;
        self.record_key(key, KeyRoute::ChainToSlot(in_is_a, id), width, height);
        Some(KeyedMatte(id))
    }

    /// Draw `key`'s matte from `from`, a kept picture, into a pool slot of
    /// its own, recorded in the chain's encoder: a grade secondary's key,
    /// drawn just before its gate reads it (M3 step 17b). `None` as
    /// [`Self::key_into_matte_slot`].
    pub fn key_kept_into_matte_slot(&mut self, key: &GpuKey, from: &KeptPicture, width: u32, height: u32) -> Option<KeyedMatte> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        if matches!(key, GpuKey::DifferenceKey { .. }) && self.plate_buf.is_none() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Matte }, |k| slot_buffer(device, k)).ok()?;
        self.record_key(key, KeyRoute::KeptToSlot(from.0, id), width, height);
        Some(KeyedMatte(id))
    }

    /// A window's matte (`masks`, as drawn now) in a pool slot of its own,
    /// for a grade secondary's gate (M3 step 17b). Drawn by a pass
    /// submitted on its own, ahead of the open encoder, so the slot is one
    /// no pass of that encoder has used. `None` when the budget refuses.
    pub fn mask_into_matte_slot(&mut self, width: u32, height: u32, masks: &[GpuMaskDef]) -> Option<KeyedMatte> {
        if self.is_device_lost() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease_for_upload(SlotKey { width, height, usage: SlotUsage::Matte }, |k| slot_buffer(device, k)).ok()?;
        self.slot_pool.mark_cpu_written(id);
        let (records, points) = Self::build_mask_storage(masks);
        let records_bytes = (records.len() * std::mem::size_of::<MaskRecord>()) as u64;
        let points_bytes = (points.len() * std::mem::size_of::<[f32; 2]>()) as u64;
        self.ensure_mask_storage_buffers(records_bytes, points_bytes);
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.dispatch_mask_v2(width, height, &records, &points, masks.len() as u32, self.slot_pool.buffer(id), workgroups);
        Some(KeyedMatte(id))
    }

    /// A keyed matte's pixels, read back for a reader off the chain.
    /// `None` while the chain that draws it is still being recorded.
    pub fn read_keyed_matte(&self, matte: &KeyedMatte, pixel_count: usize) -> Option<Vec<f32>> {
        if self.is_device_lost() || self.chain_encoder.is_some() {
            return None;
        }
        Some(self.download_linear_rgba_f32(self.slot_pool.buffer(matte.0), pixel_count)).filter(|v| v.len() == pixel_count * 4)
    }

    /// Give a keyed matte's slot back: its last reader is recorded.
    pub fn release_keyed_matte(&mut self, matte: KeyedMatte) {
        self.slot_pool.release(matte.0);
    }

    /// Record `key`'s kernel along `route`.
    pub(super) fn record_key(&mut self, key: &GpuKey, route: KeyRoute, width: u32, height: u32) {
        let slot = self.next_params_slot();
        let pipeline = match *key {
            GpuKey::Qualifier { hue_center, hue_width, hue_soft, sat_low, sat_high, sat_soft, lum_low, lum_high, lum_soft, invert, rgb, volume } => {
                let (mode, rgb, v) = GpuQualifierParams::shape(rgb, volume);
                self.write_chain_params(slot, &GpuQualifierParams {
                    width,
                    height,
                    hue_center: hue_center / 360.0,
                    hue_width: hue_width / 360.0,
                    hue_soft: hue_soft / 360.0,
                    sat_low,
                    sat_high,
                    sat_soft,
                    lum_low,
                    lum_high,
                    lum_soft,
                    invert: invert as u32,
                    sec_exposure: 0.0,
                    sec_saturation: 1.0,
                    sec_hue_shift: 0.0,
                    sec_temperature: 0.0,
                    matte_preview: 1,
                    mode,
                    _pad1: 0,
                    _pad2: 0,
                    rgb,
                    _pad3: 0.0,
                    keep_mean: v[0],
                    keep_inv: [v[1], v[2], v[3]],
                    remove_mean: v[4],
                    remove_inv: [v[5], v[6], v[7]],
                    reach: v[8],
                });
                &self.pipelines.qualifier_pipeline
            }
            GpuKey::LumaKey { low, high, soft, invert } => {
                self.write_chain_params(slot, &GpuLumaKeyParams {
                    width, height, low, high, soft,
                    invert: invert as u32,
                    into_alpha: 0,
                    _pad1: 0,
                    luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
                });
                &self.pipelines.luma_key_pipeline
            }
            GpuKey::DifferenceKey { tolerance, soft, invert, luminance } => {
                self.write_chain_params(slot, &GpuDifferenceKeyParams {
                    width,
                    height,
                    tolerance,
                    soft,
                    invert: invert as u32,
                    match_space: luminance as u32,
                    _pad0: 0,
                    _pad1: 0,
                    luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
                });
                &self.pipelines.difference_key_pipeline
            }
        };
        let (input, output) = match route {
            KeyRoute::KeptToSlot(kept, id) => (self.slot_pool.buffer(kept), self.slot_pool.buffer(id)),
            KeyRoute::ChainToSlot(in_is_a, id) => (
                if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }.expect("chain buffers"),
                self.slot_pool.buffer(id),
            ),
        };
        let mut plate_read = false;
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
        ];
        let layout = match key {
            GpuKey::DifferenceKey { .. } => {
                entries.push(wgpu::BindGroupEntry { binding: 3, resource: self.plate_buf.as_ref().expect("checked by the caller").as_entire_binding() });
                plate_read = true;
                &self.pipelines.difference_key_bgl
            }
            _ => &self.pipelines.bind_group_layout,
        };
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("chain_bg_key"), layout, entries: &entries });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
        self.plate_read_in_chain |= plate_read;
    }

    /// Blur an alpha mask on the GPU. Input/output are single-channel u8 buffers.
    /// Uses three separable box passes at round(radius), matching title CPU masks.
    pub fn blur_alpha_gpu(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        let pixel_count = (w * h) as u64;
        // Each alpha value stored as one u32
        let byte_size = pixel_count * 4;
        self.ensure_buffers(byte_size);

        // Convert u8 alpha -> u32 for GPU
        let input_u32: Vec<u32> = mask.iter().map(|&a| a as u32).collect();

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        // 3-pass box blur for gaussian approximation
        let boxes = [radius.max(0.0).round() as usize; 3];
        let mut current_in_a = true;

        for &box_r in &boxes {
            if box_r == 0 { continue; }

            // Horizontal pass
            let (inp, out) = if current_in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            self.write_params(&AlphaBlurParams { width: w, height: h, radius: box_r as u32, _pad: 0 });
            self.dispatch(&self.pipelines.alpha_blur_h_pipeline, inp, out, workgroups);
            current_in_a = !current_in_a;

            // Vertical pass
            let (inp, out) = if current_in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            self.dispatch(&self.pipelines.alpha_blur_v_pipeline, inp, out, workgroups);
            current_in_a = !current_in_a;
        }

        let result_buf = if current_in_a { buf_a } else { buf_b };
        let result_bytes = self.download(result_buf, byte_size);

        // Convert u32 back to u8
        let result_u32: &[u32] = bytemuck::cast_slice(&result_bytes);
        result_u32.iter().map(|&v| v.min(255) as u8).collect()
    }

    /// Dilate an alpha mask on the GPU using circular distance.
    pub fn dilate_alpha_gpu(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        let r = radius.ceil() as u32;
        if r == 0 { return mask.to_vec(); }

        let pixel_count = (w * h) as u64;
        let byte_size = pixel_count * 4;
        self.ensure_buffers(byte_size);

        let input_u32: Vec<u32> = mask.iter().map(|&a| a as u32).collect();

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        self.write_params(&AlphaDilateParams { width: w, height: h, radius: r, radius_sq: (radius.max(0.0) * radius.max(0.0)) as u32 });
        self.dispatch(&self.pipelines.alpha_dilate_pipeline, buf_a, buf_b, workgroups);

        let result_bytes = self.download(buf_b, byte_size);
        let result_u32: &[u32] = bytemuck::cast_slice(&result_bytes);
        result_u32.iter().map(|&v| v.min(255) as u8).collect()
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MatteExtractParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) channel: u32,
    pub(super) invert: u32,
    pub(super) luma_r: f32,
    pub(super) luma_g: f32,
    pub(super) luma_b: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MaskPoolParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) radius: u32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct AlphaFormParams {
    pub(super) width: u32,
    pub(super) height: u32,
    /// 0 premultiplies, 1 divides back to straight.
    pub(super) unpremultiply: u32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ScaleAlphaParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) factor: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ViewChannelParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) channel: u32,
    pub(super) _pad0: u32,
    pub(super) luma: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ChromaKeyParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) key_cb: f32,
    pub(super) key_cr: f32,
    pub(super) tolerance: f32,
    pub(super) soft_edge: f32,
    /// Spill method as `u32` (0 = None, 1 = ChannelClamp, 2 = Hawk).
    /// WGSL uniforms can't carry Rust enums directly so we encode here.
    pub(super) spill_method: u32,
    pub(super) spill_strength: f32,
    /// Index of the dominant key channel (0 = R, 1 = G, 2 = B).
    /// Pre-computed CPU-side from `key_color` so the shader avoids a
    /// per-pixel argmax. Tied to `dominant_channel` in chroma_key.rs.
    pub(super) spill_key_idx: u32,
    /// Round to 16-byte alignment — WGSL uniforms require it.
    pub(super) _pad: [u32; 3],
}

/// A gate's blend: its mix, and which channel of its matte is the
/// coverage (`MaskChannel` as numbered: red, luminance, alpha), with the
/// working space's luma weights for the second.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuApplyMatteParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mix: f32,
    pub(super) channel: u32,
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    /// 1: the matte taken inverted, an outside node's key (#898). The
    /// word that padded the struct to 32 bytes; the size is unchanged.
    pub(super) invert: u32,
}

pub(super) const SHADER_CHROMA_KEY: &str = gpu_shader!("mattes/chroma_key.wgsl");

// A matte from a layer: `track_matte::extract_mask_linear` — the source
// is premultiplied, so colour channels are divided by alpha first;
// channel 0 alpha, 1 luminance, 2 red, 3 green, 4 blue; then inverted.
// Written to every channel of the matte buffer.
pub(super) const SHADER_MATTE_EXTRACT: &str = gpu_shader!("mattes/matte_extract.wgsl");

// A choke: the minimum over a (2r+1)-wide window, one axis per pass.
pub(super) const SHADER_MASK_MIN_H: &str = gpu_shader!("mattes/mask_min_h.wgsl");

pub(super) const SHADER_MASK_MIN_V: &str = gpu_shader!("mattes/mask_min_v.wgsl");

// The matte into a layer's alpha, in place.
pub(super) const SHADER_MATTE_APPLY: &str = gpu_shader!("mattes/matte_apply.wgsl");

// A layer's alpha times a factor, in place. Binding 0 is unused.
pub(super) const SHADER_SCALE_ALPHA: &str = gpu_shader!("mattes/scale_alpha.wgsl");

// The viewer's channel view of the finished stack (#2758), flattened as
// the premultiply is: the picture, one of red, green, blue in grey, the
// alpha before the flatten, or the luma by the working space's row. The
// CPU twin is `composite::view_channel`.
pub(super) const SHADER_VIEW_CHANNEL: &str = gpu_shader!("mattes/view_channel.wgsl");

// A layer premultiplied keeping its alpha, or divided back to straight:
// what a transition mixes in, as the CPU mixes it (#1026).
pub(super) const SHADER_ALPHA_FORM: &str = concat!(
    // Two parts around the alpha floor, which the shader uses inside an
    // expression: an include can only stand for whole lines (#2890).
    gpu_shader!("mattes/alpha_form.1.wgsl"),
    unpremultiply_min_alpha_wgsl!(),
    gpu_shader!("mattes/alpha_form.2.wgsl"),
);

// Qualifier — HSL-based isolation + secondary colour correction on
// matched pixels. HSL / hue-wheel values are now in linear light
// (grey lum = 0.18 rather than 0.46). The sec_exposure adjustment
// now acts as a physically correct 2^stops multiplier on the
// isolated region.
pub(super) const SHADER_QUALIFIER: &str = gpu_shader!("mattes/qualifier.wgsl");

// ─── Luma Key GPU Shader ────────────────────────────────────────────────────
//
// Standalone luminance-threshold key. Mirrors `SHADER_QUALIFIER`'s
// matte-producer contract (greyscale RGBA output) but evaluates
// Rec.709 luma alone — no HSL machinery, no secondary grade. Used
// by the `NodeKind::LumaKey` graph node when GPU is available.
//
// **Colorspace note:** Rec.709 luma weights are technically wrong
// for AP1 (ACEScg working colorspace) — should be ~0.272/0.674/0.054
// there. Matches Qualifier's behaviour; both will be re-tuned
// post-Phase-E.
// ─── Difference Key GPU Shader ──────────────────────────────────────────────
//
// The CPU twin is `klipp_color::difference_key::generate_difference_key_matte`.
// Change one, change the other; `difference_key_gpu_parity` holds them
// together.
//
// Unlike the luma key, this keys *linear* light on both sides, because
// the CPU function is handed the linear frame and the linear plate and
// compares them directly. Encoding either here would make the same
// tolerance mean a different distance depending on which path ran.
pub(super) const SHADER_DIFFERENCE_KEY: &str = gpu_shader!("mattes/difference_key.wgsl");

pub(super) const SHADER_LUMA_KEY: &str = gpu_shader!("mattes/luma_key.wgsl");

// Alpha-only shaders use the shared buf_a/buf_b storage, but view them
// through a dedicated u32-per-pixel layout (one pixel per u32, no RGBA
// packing). That's fine even though buf_a/b are sized for vec4<f32> —
// the shader just touches the first `w*h` u32 elements.
pub(super) const SHADER_ALPHA_BLUR_H: &str = gpu_shader!("mattes/alpha_blur_h.wgsl");

pub(super) const SHADER_ALPHA_BLUR_V: &str = gpu_shader!("mattes/alpha_blur_v.wgsl");

pub(super) const SHADER_ALPHA_DILATE: &str = gpu_shader!("mattes/alpha_dilate.wgsl");

// ── Mask matte generation (SDF-based power windows, v2 storage layout) ─────
//
// All per-mask shape data lives in a storage buffer (`records`),
// pooled bezier control points live in a second storage buffer
// (`points`), and the uniform only carries frame constants. This
// removes the previous 4-mask cap and unblocks Bezier shapes —
// which can have arbitrary point counts and so don't fit in a
// fixed-size uniform.
//
// Coverage value (`matte`) is written to all RGB channels as a 0..1
// alpha. The CPU readback path uses `download_raw_rgba_as_u8` (no
// sRGB transform); writing to the linear storage buffer is a no-op
// conversion as long as readers don't gamma-encode it.
pub(super) const SHADER_MASK_V2: &str = gpu_shader!("mattes/mask_v2.wgsl");

/// Mask cutout — multiply chain RGB by matte's R channel and force
/// alpha=1.0 so the egui viewer (which expects opaque RGBA) stays
/// happy. Feathered mask edges fall out of the multiply for free.
/// 4-binding layout matches `clarity_apply` so we can reuse the same
/// `huesat_bgl` and `huesat_pl` for the pipeline.
pub(super) const SHADER_MASK_CUTOUT: &str = gpu_shader!("mattes/mask_cutout.wgsl");

// Grade mix — a grade's result mixed with the picture it started from,
// kept in binding 3: the Corrector's mix, over all its stages.
/// ApplyMatte as a chain dispatch: the graded frame where the matte
/// selects, the frame kept from before the secondary where it does not.
///
/// `mix` scales the whole matte, so a secondary can be dialled back
/// without touching the key. The matte's coverage is its red channel
/// unless its correction picks luminance or alpha (M3 step 17a), the
/// product clamped to [0, 1], and alpha is left at 1.0 — the same
/// conventions the CPU twin in `klipp_nodes::executor` follows, because
/// the two are compared against each other.
pub(super) const SHADER_APPLY_MATTE_CHAIN: &str = gpu_shader!("mattes/apply_matte_chain.wgsl");
