//! The layer stack on the GPU (#2890): the canvas a frame's layers are
//! blended onto, its accumulator and layer slots, and pictures kept between
//! passes.

use super::*;

impl GpuEffectsPipeline {
    /// Ready the stack for a frame of `width` × `height` with room for
    /// `layers` layers. Buffers grow to the largest request and stay.
    pub fn stack_begin(&mut self, width: u32, height: u32, layers: usize) -> bool {
        // Whatever the last frame left goes to the queue; this frame's
        // passes record into one encoder from here.
        self.stack_end();
        self.title_metal_shots.clear();
        self.batching.set(true);
        self.stack_params_slot.set(0);
        if self.is_device_lost() || width == 0 || height == 0 {
            return false;
        }
        let byte_size = (width as u64) * (height as u64) * 16;
        if byte_size > self.stack_buf_size {
            self.stack_layers.clear();
            self.stack_acc = None;
            self.stack_buf_size = byte_size;
        }
        // Every buffer in the pool is the pool's size, never this frame's:
        // a layer first wanted by a smaller frame (a 2128-row clip after a
        // 2132-row one) would otherwise be short for the next larger
        // frame, whose copies then overrun it — a validation error that
        // took the GPU away for the rest of the session.
        let pool_size = self.stack_buf_size;
        let make = |device: &wgpu::Device, label: &str| {
            std::sync::Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: pool_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }))
        };
        while self.stack_layers.len() < layers {
            let label = format!("stack_layer_{}", self.stack_layers.len());
            self.stack_layers.push(make(&self.device, &label));
        }
        if self.stack_acc.is_none() {
            self.stack_acc = Some(make(&self.device, "stack_acc"));
        }
        // The chain buffers must exist at this size too: layers copy out
        // of them, effects run in them, the finish copies into them.
        self.ensure_buffers(byte_size);
        if self.stack_dims != (width, height) {
            self.title_style_scratch = None;
            self.title_post_scratch = None;
            self.title_scene_bounds = None;
            self.stack_sprites.clear();
        }
        self.stack_title_slot = layers;
        self.stack_dims = (width, height);
        self.stack_acc_touched = false;
        true
    }

    pub(super) fn stack_pixel_count(&self) -> u64 {
        (self.stack_dims.0 as u64) * (self.stack_dims.1 as u64)
    }

    /// A stack frame is over: its passes go to the queue and passes
    /// submit themselves again until the next [`Self::stack_begin`].
    pub fn stack_end(&self) {
        self.flush();
        self.batching.set(false);
    }

    /// One compute pass over `workgroups`, bound as (read, read_write,
    /// params) on the effects layout.
    pub(super) fn stack_pass(&self, pipeline: &wgpu::ComputePipeline, read: &wgpu::Buffer, rw: &wgpu::Buffer, workgroups: u32, label: &str) {
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: read.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: rw.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(Some(label));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(enc);
    }

    /// A whole-frame copy between two of the stack's buffers. `false`,
    /// and nothing recorded, when either is too small for the frame:
    /// an overrunning copy is a validation error, and the frame is
    /// better declined than the device marked bad.
    pub(super) fn stack_copy(&self, from: &wgpu::Buffer, to: &wgpu::Buffer, label: &str) -> bool {
        let bytes = self.stack_pixel_count() * 16;
        if from.size() < bytes || to.size() < bytes {
            error!(
                "GPU stack: {label} needs {bytes} bytes but has {} to copy from and {} to copy into; the frame is declined",
                from.size(),
                to.size()
            );
            return false;
        }
        let mut enc = self.encoder(Some(label));
        enc.copy_buffer_to_buffer(from, 0, to, 0, bytes);
        self.submit(enc);
        true
    }

    /// The accumulator, a canvas if nothing has been laid on it yet,
    /// for a chain to seed from (an adjustment layer).
    pub fn stack_acc_buffer(&mut self) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if !self.stack_ensure_acc() {
            return None;
        }
        self.flush();
        self.stack_acc.clone()
    }

    /// Layer `i` cleared to transparent black.
    pub fn stack_layer_clear(&mut self, i: usize) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let mut enc = self.encoder(Some("stack_layer_clear"));
        enc.clear_buffer(&self.stack_layers[i], 0, Some(self.stack_pixel_count() * 16));
        self.submit(enc);
        true
    }

    /// Layer `dst` becomes a copy of layer `src`, every pixel.
    pub fn stack_layer_copy(&mut self, src: usize, dst: usize) -> bool {
        if self.is_device_lost() || src >= self.stack_layers.len() || dst >= self.stack_layers.len() || src == dst {
            return false;
        }
        let mut enc = self.encoder(Some("stack_layer_copy"));
        enc.copy_buffer_to_buffer(&self.stack_layers[src], 0, &self.stack_layers[dst], 0, self.stack_pixel_count() * 16);
        self.submit(enc);
        true
    }

    /// Layer `i`'s buffer, for a chain to seed from: a layer whose effects
    /// run as a plan of its own after it was placed (a title's sprite, a
    /// generator; phase 2b M2 step 11d), its result copied back with
    /// [`Self::stack_layer_from_chain`].
    pub fn stack_layer_buffer(&self, i: usize) -> Option<std::sync::Arc<wgpu::Buffer>> {
        self.stack_layers.get(i).cloned()
    }

    /// Layer `i` takes the chain's output slot (a grade's result, or a
    /// seeded frame), as an on-GPU copy.
    pub fn stack_layer_from_chain(&mut self, i: usize, slot_a: bool) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let src = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let Some(src) = src else { return false };
        self.stack_copy(src, &self.stack_layers[i], "stack_layer_from_chain")
    }

    /// A linear frame sent up once and kept on the GPU (#2092): a still
    /// image's picture, seeded from on later frames instead of uploaded
    /// again. `None` when the device is lost.
    pub fn kept_linear(&mut self, linear_rgba: &[f32]) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if self.is_device_lost() || linear_rgba.is_empty() {
            return None;
        }
        let kept = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kept_still"),
            size: std::mem::size_of_val(linear_rgba) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.upload(&kept, 0, bytemuck::cast_slice(linear_rgba));
        Some(std::sync::Arc::new(kept))
    }

    /// Layer `i` takes a linear working-space frame from the CPU.
    pub fn stack_layer_from_linear(&mut self, i: usize, linear_rgba: &[f32]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        if linear_rgba.len() as u64 != self.stack_pixel_count() * 4 {
            return false;
        }
        self.upload(&self.stack_layers[i], 0, bytemuck::cast_slice(linear_rgba));
        true
    }

    /// The frame's canvas, clear where nothing has been laid on it: an
    /// empty frame (a gap, a strobe's dark pieces) is this canvas, graded
    /// and finished as any composite is (#2377).
    pub fn stack_clear_canvas(&mut self) -> bool {
        !self.is_device_lost() && self.stack_ensure_acc()
    }

    pub(super) fn stack_ensure_acc(&mut self) -> bool {
        let Some(acc) = self.stack_acc.clone() else { return false };
        if !self.stack_acc_touched {
            let mut enc = self.encoder(Some("stack_clear"));
            enc.clear_buffer(&acc, 0, Some(self.stack_pixel_count() * 16));
            self.submit(enc);
            self.stack_acc_touched = true;
        }
        true
    }

    /// The accumulator into chain slot A, for an adjustment layer's
    /// chain to run on.
    pub fn stack_acc_to_chain(&mut self) -> bool {
        if !self.stack_ensure_acc() {
            return false;
        }
        let acc = self.stack_acc.clone().unwrap();
        self.stack_copy(&acc, self.buf_a.as_ref().unwrap(), "stack_acc_to_chain")
    }

    /// Chain slot B into slot A, so an effects run can start from A.
    pub fn stack_chain_to_slot_a(&mut self, slot_a: bool) {
        if !slot_a {
            self.stack_copy(self.buf_b.as_ref().unwrap(), self.buf_a.as_ref().unwrap(), "stack_chain_b_to_a");
        }
    }

    /// Mix the chain's slot back into the accumulator: acc = acc + (chain
    /// - acc) × opacity, every channel — an adjustment layer's opacity.
    pub fn stack_mix_from_chain(&mut self, slot_a: bool, opacity: f32) -> bool {
        if !self.stack_ensure_acc() {
            return false;
        }
        let (width, height) = self.stack_dims;
        self.write_params(&BlendLayerParams { width, height, opacity, mode: 13, y0: 0, y1: height, _p0: 0, _p1: 0 });
        let src = if slot_a { self.buf_a.as_ref().unwrap() } else { self.buf_b.as_ref().unwrap() };
        let acc = self.stack_acc.clone().unwrap();
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        self.stack_pass(&self.pipelines.blend_layer_pipeline, src, &acc, wg, "stack_mix");
        true
    }

    /// Close the stack: the accumulator (premultiplied unless
    /// `keep_alpha`) moves into chain slot A, where every encode and
    /// the LUT can take it. Returns false when nothing was blended.
    pub fn stack_finish_into_chain(&mut self, keep_alpha: bool) -> bool {
        if self.is_device_lost() || !self.stack_acc_touched {
            return false;
        }
        let (width, height) = self.stack_dims;
        let acc = self.stack_acc.clone().unwrap();
        if keep_alpha {
            return self.stack_copy(&acc, self.buf_a.as_ref().unwrap(), "stack_finish_copy");
        }
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        self.stack_pass(&self.pipelines.premultiply_pipeline, &acc, self.buf_a.as_ref().unwrap(), wg, "stack_premultiply");
        true
    }

    /// The stack finished for the viewer's channel view (#2758):
    /// `stack_finish_into_chain(false)` with `channel` (as
    /// `composite::view_channel` takes it) shown in grey.
    pub fn stack_finish_viewing(&mut self, channel: u32, luma: [f32; 3]) -> bool {
        if self.is_device_lost() || !self.stack_acc_touched {
            return false;
        }
        let (width, height) = self.stack_dims;
        let acc = self.stack_acc.clone().unwrap();
        self.write_params(&ViewChannelParams { width, height, channel, _pad0: 0, luma: [luma[0], luma[1], luma[2], 0.0] });
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        self.stack_pass(&self.pipelines.view_channel_pipeline, &acc, self.buf_a.as_ref().unwrap(), wg, "stack_view_channel");
        true
    }

    /// The finished stack (`stack_finish_into_chain` with `keep_alpha`)
    /// kept in a buffer of its own (#2757): a precomposition's scene,
    /// composed before its parent's stack, which seeds the parent's layer
    /// from it. `reuse` is the buffer kept for the clip last frame, taken
    /// again when it is the frame's size. Recorded in the frame's encoder,
    /// so it lands before the next stack's passes.
    pub fn stack_keep_chain(&mut self, reuse: Option<std::sync::Arc<wgpu::Buffer>>) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if self.is_device_lost() {
            return None;
        }
        let bytes = self.stack_pixel_count() * 16;
        let kept = match reuse.filter(|b| b.size() == bytes) {
            Some(b) => b,
            None => std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kept_scene"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })),
        };
        self.stack_copy(self.buf_a.as_ref()?, &kept, "stack_keep_scene").then_some(kept)
    }

    /// The finished stack as linear working-space pixels, for tests and
    /// the CPU twin comparison. Call after `stack_finish_into_chain`.
    pub fn stack_download_linear(&self) -> Vec<f32> {
        self.download_chain_output_linear(true, self.stack_pixel_count() as usize)
    }

    /// The stack's composite so far in a pool slot of its own (#2758):
    /// what is below a layer that reads it, copied out before that layer is
    /// placed, for its run to read as its side picture. Held until that
    /// run's slots are given back. `None` without a stack or a slot.
    pub fn stack_acc_as_picture(&mut self) -> Option<KeptPicture> {
        if self.is_device_lost() || !self.stack_ensure_acc() {
            return None;
        }
        let (width, height) = self.stack_dims;
        let acc = self.stack_acc.clone()?;
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        let mut enc = self.encoder(Some("stack_acc_as_picture"));
        enc.copy_buffer_to_buffer(&acc, 0, self.slot_pool.buffer(id), 0, u64::from(width) * u64::from(height) * 16);
        self.submit(enc);
        Some(KeptPicture(id))
    }

    /// Layer `slot` of the stack in a pool slot of its own (#2758): a title
    /// drawn there as a layer is, handed to a clip's run as its side
    /// picture. `None` without that layer or a slot.
    pub fn stack_layer_as_picture(&mut self, slot: usize) -> Option<KeptPicture> {
        if self.is_device_lost() {
            return None;
        }
        let (width, height) = self.stack_dims;
        let layer = self.stack_layers.get(slot)?.clone();
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        let mut enc = self.encoder(Some("stack_layer_as_picture"));
        enc.copy_buffer_to_buffer(&layer, 0, self.slot_pool.buffer(id), 0, u64::from(width) * u64::from(height) * 16);
        self.submit(enc);
        Some(KeptPicture(id))
    }

    /// Keep the chain's picture (in `buf_a` when `in_is_a`) in a pool slot
    /// of its own, copied in the chain's encoder, so it is ordered against
    /// the passes around it. `None` when there is no chain or the pool's
    /// budget refuses.
    pub fn keep_picture(&mut self, in_is_a: bool, width: u32, height: u32) -> Option<KeptPicture> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        let size = u64::from(width) * u64::from(height) * 16;
        let src = if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }.expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        enc.copy_buffer_to_buffer(src, 0, self.slot_pool.buffer(id), 0, size);
        Some(KeptPicture(id))
    }

    /// `px`, a picture from the CPU (a composite's second picture, read
    /// back or the source frame), in a pool slot of its own. Uploaded, so
    /// it lands before the open encoder: the slot is one no pass of it has
    /// used, held after its return until that encoder is submitted. `None`
    /// when the size is wrong or the budget refuses.
    pub fn upload_picture(&mut self, px: &[f32], width: u32, height: u32) -> Option<KeptPicture> {
        if self.is_device_lost() || (px.len() * 4) as u64 != u64::from(width) * u64::from(height) * 16 {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease_for_upload(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        self.upload_frame(self.slot_pool.buffer(id), bytemuck::cast_slice(px));
        self.slot_pool.mark_cpu_written(id);
        Some(KeptPicture(id))
    }

    /// A kept picture's pixels, read back for a reader off the chain.
    /// `None` while the chain that keeps it is still being recorded.
    pub fn read_picture(&self, picture: &KeptPicture, pixel_count: usize) -> Option<Vec<f32>> {
        if self.is_device_lost() || self.chain_encoder.is_some() {
            return None;
        }
        Some(self.download_linear_rgba_f32(self.slot_pool.buffer(picture.0), pixel_count)).filter(|v| v.len() == pixel_count * 4)
    }

    /// Give a kept picture's slot back: its last reader is recorded.
    pub fn release_picture(&mut self, picture: KeptPicture) {
        self.slot_pool.release(picture.0);
    }
}


