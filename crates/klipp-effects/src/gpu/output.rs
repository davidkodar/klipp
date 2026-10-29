//! Output and input on the GPU (#2890): the chain encoded for display
//! (SDR, HDR, the monitor's view), to YUV 4:2:0 for the encoder, to the
//! ACES and BT.2020 deliveries, and frames bridged in from u8, RGBA64 and
//! HDR sources, with their params, tables and shaders. The public shapes
//! (`Yuv420Target`, `SceneReferredEnd`, `GpuFrameLight`, `fit_rect`)
//! stay in `gpu`.

use super::*;

impl GpuEffectsPipeline {
    pub fn linear_rgba_to_u8_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32) -> Option<Vec<u8>> {
        if self.is_device_lost() { return None; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return Some(Vec::new()); }
        let in_bytes = pixel_count * 16;     // vec4<f32> per pixel
        let out_bytes = pixel_count * 4;     // u8 RGBA per pixel
        if linear_rgba.len() < (pixel_count * 4) as usize {
            // Caller passed a short buffer — fall through to CPU.
            return None;
        }

        // Reuse the standard chain input buffer for the linear data.
        // It's already the right size for the effects pipeline; if a
        // chain is mid-flight we'd clobber it, but this method is only
        // called from the engine's wrap path which runs serially with
        // the executor's flush_chain.
        self.ensure_buffers(in_bytes);

        // Resize the dedicated u8 output + staging buffers as needed.
        if self.bridge_u8_out_size < out_bytes {
            self.bridge_u8_out_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_out"),
                size: out_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.bridge_u8_out_size = out_bytes;
        }
        if self.bridge_u8_staging_size < out_bytes {
            self.bridge_u8_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_staging"),
                size: out_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.bridge_u8_staging_size = out_bytes;
        }

        // Take refs after potential mutation above so the borrow
        // checker is happy.
        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        let out_buf = self.bridge_u8_out_buf.as_ref().expect("bridge_u8_out_buf just created");
        let staging = self.bridge_u8_staging_buf.as_ref().expect("bridge_u8_staging_buf just created");

        // Upload input + write params.
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        // Bind group: input(buf_a, read), output(out_buf, write), params(uniform).
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_u8_bg"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });

        // Single submit: dispatch compute + copy_buffer_to_buffer for
        // readback. Same one-submit pattern as flush_chain — halves
        // NVIDIA Vulkan driver overhead vs two separate submits.
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_u8_enc"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("bridge_u8_pass"), timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.bridge_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        let mut readback = self.readbacks.begin(out_bytes);
        encoder.copy_buffer_to_buffer(out_buf, 0, staging, 0, out_bytes);
        self.submit(encoder);

        mark_gpu_stage(GpuStage::LinearRgbaToU8);
        let slice = staging.slice(..out_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        self.flush();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        // Non-blocking poll loop instead of `Maintain::Wait` — on a
        // UI-shared device, Wait never returns because eframe-wgpu
        // keeps the queue non-empty with continuous egui submits.
        // `poll_until_mapped` drives the map callback via Poll until
        // it fires (or times out at 2 s).
        let result = match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = slice.get_mapped_range();
                let result = data.to_vec();
                drop(data);
                staging.unmap();
                readback.succeeded = true;
                Some(result)
            }
            other => {
                self.mark_device_lost(&format!("bridge_u8 map_async failed: {other:?}"));
                None
            }
        };
        mark_gpu_stage(GpuStage::Idle);
        result
    }

    /// Encode an externally-provided linear-f32 RGBA buffer directly
    /// into a `wgpu::Texture`. Same output as
    /// `encode_grade_to_texture` but input is a CPU slice uploaded
    /// to `buf_a` here — useful at the legacy post_process_rgba
    /// boundary that still downloads the grade chain's result as
    /// `Vec<f32>`. Later: the chain skips the download entirely and
    /// `encode_grade_to_texture` reads the slot directly.
    ///
    /// Same target constraints as `encode_grade_to_texture`:
    /// `Rgba8Unorm`, matching size, `width * 4 % 256 == 0`.
    pub fn linear_rgba_to_texture_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        target: &wgpu::Texture,
    ) -> bool {
        let pixel_count = (width as u64) * (height as u64);
        if linear_rgba.len() < (pixel_count * 4) as usize {
            return false;
        }
        let byte_size = pixel_count * 16;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        self.encode_grade_to_texture(true, width, height, target)
    }

    /// Encode the grade chain's final linear-f32 output (currently in
    /// `buf_a` when `slot_a = true`, `buf_b` otherwise) to sRGB-u8
    /// RGBA, then copy the result into the caller-supplied
    /// `wgpu::Texture`. No CPU readback — the texture is immediately
    /// bindable by another consumer on the same device
    /// (egui-wgpu's viewer, for example).
    ///
    /// The target texture must be `Rgba8Unorm` (not sRGB — our
    /// shader output bytes are already sRGB-encoded, we don't want a
    /// second decode-on-sample) with `TEXTURE_BINDING | COPY_DST`
    /// usage and dimensions `width × height`. Misconfigured targets
    /// return `false` without dispatching.
    ///
    /// Returns `false` on device-lost / misconfigured target; the
    /// caller's CPU-download + egui-upload fallback should kick in.
    pub fn encode_grade_to_texture(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: &wgpu::Texture,
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        let out_bytes = pixel_count * 4;

        // Target validation — surface misconfigurations loudly
        // instead of corrupting the display.
        let size = target.size();
        if size.width != width || size.height != height {
            tracing::warn!(
                "encode_grade_to_texture: target size {}x{} != requested {width}x{height}",
                size.width, size.height,
            );
            return false;
        }
        if target.format() != wgpu::TextureFormat::Rgba8Unorm {
            tracing::warn!(
                "encode_grade_to_texture: target format {:?} must be Rgba8Unorm",
                target.format(),
            );
            return false;
        }
        // wgpu's `copy_buffer_to_texture` requires `bytes_per_row` to
        // be a multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256).
        // Our output buffer is tightly packed (width * 4 bytes/row),
        // so any width whose byte pitch isn't a multiple of 256
        // would silently corrupt the copy under the release-build
        // validation disable. Reject up front.
        let bytes_per_row = width * 4;
        if !bytes_per_row.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) {
            tracing::warn!(
                "encode_grade_to_texture: width {width} (pitch {bytes_per_row} B) \
                 not a multiple of COPY_BYTES_PER_ROW_ALIGNMENT={}. \
                 Real video frames ≥ 64 px wide always meet this.",
                wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
            );
            return false;
        }

        // Need our reusable u8 output buffer at the right size.
        if self.bridge_u8_out_size < out_bytes {
            self.bridge_u8_out_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_out"),
                size: out_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.bridge_u8_out_size = out_bytes;
        }

        let src_buf = if slot_a {
            self.buf_a.as_ref()
        } else {
            self.buf_b.as_ref()
        }.expect("chain buffers must be initialised before encode_grade_to_texture");
        let out_buf = self.bridge_u8_out_buf.as_ref().unwrap();

        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("encode_grade_to_texture_bg"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("encode_grade_to_texture"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.pipelines.bridge_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        // Copy the packed-u32 buffer straight into the texture. The
        // buffer layout matches `Rgba8Unorm`'s row-major RGBA bytes
        // byte-for-byte (R in bits 0-7 of each u32, then G, B, A).
        encoder.copy_buffer_to_texture(
            wgpu::ImageCopyBuffer {
                buffer: out_buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
            },
            wgpu::ImageCopyTexture {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.submit(encoder);
        true
    }

    /// GPU bridge: packed sRGB-u8 RGBA (`array<u32>`) → linear-f32
    /// RGBA (`array<vec4<f32>>`). Reads an *external* input buffer
    /// (typically the output of `convert_*_to_rgba_on_gpu`) and writes
    /// the decoded linear pixels into the pipeline's chain slot
    /// `buf_a`, leaving the chain primed so downstream grade-node
    /// dispatches can consume it.
    ///
    /// Fully GPU-resident — no CPU bytes touched, no
    /// `queue.write_buffer` round-trip. This is the operation that
    /// eliminates the ~5 ms PCIe upload the CPU path paid on every
    /// frame.
    ///
    /// Returns `false` if the GPU device is lost or pre-conditions
    /// aren't met (chain buffers not allocated, input buffer undersized
    /// for the declared dimensions). Callers should fall back to the
    /// CPU `u8_rgba_to_linear_f32` → `upload_chain_input_linear`
    /// path in that case.
    pub fn bridge_u8_to_linear_into_chain(
        &mut self,
        input_u8: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        let linear_byte_size = pixel_count * 16;
        self.ensure_buffers(linear_byte_size);

        // Input must have at least 4 bytes per pixel.
        if input_u8.size() < pixel_count * 4 { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_u8_to_linear_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                // binding 0: input u8 buffer (external, read-only)
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                // binding 1: output linear buffer (chain's buf_a, read-write)
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                // binding 2: params uniform
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                // binding 3: the codes' decode table
                wgpu::BindGroupEntry { binding: 3, resource: self.pipelines.srgb8_lut.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_u8_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.pipelines.bridge_u8_to_linear_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        true
    }

    /// [`Self::bridge_u8_to_linear_into_chain`] from a frame of another
    /// size: `src_w` by `src_h` sRGB bytes fitted into the chain's
    /// `width` by `height` the way the CPU compositor fits a source —
    /// one scale factor, centred, the rest transparent; an area average
    /// when shrinking by two or more, bilinear otherwise, both in linear
    /// light.
    ///
    /// A 1440p overlay on a 1080p timeline used to send every frame it
    /// was on down to the CPU path, a readback and an upload each: 24 ms
    /// a frame against one.
    pub fn bridge_u8_fit_to_linear_into_chain(
        &mut self,
        input_u8: &wgpu::Buffer,
        src_w: u32,
        src_h: u32,
        width: u32,
        height: u32,
    ) -> bool {
        if self.is_device_lost() || src_w == 0 || src_h == 0 || width == 0 || height == 0 {
            return false;
        }
        if input_u8.size() < (src_w as u64) * (src_h as u64) * 4 {
            return false;
        }
        let pixel_count = (width as u64) * (height as u64);
        self.ensure_buffers(pixel_count * 16);
        let (dst_w, dst_h, x_off, y_off) = fit_rect(src_w, src_h, width, height);
        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&BridgeU8FitParams { src_w, src_h, out_w: width, out_h: height, dst_w, dst_h, x_off, y_off });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_u8_fit_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.pipelines.srgb8_lut.as_entire_binding() },
            ],
        });
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_u8_fit_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.bridge_u8_fit_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        true
    }

    /// The chain's graded frame straight to 8-bit 4:2:0 planes: Y, then
    /// Cb, then Cr, tightly packed, ready for the encoder.
    ///
    /// The whole point is what does *not* happen — the linear frame never
    /// comes down. At 4K that is 132 MB replaced by 12.4 MB, and the
    /// per-pixel CPU display-and-convert pass replaced by nothing.
    ///
    /// `None` when this is not the right route and the caller must use
    /// `encode_linear_direct`: no GPU, a device lost, dimensions the
    /// block layout cannot tile (width not a multiple of 8, height odd),
    /// or a transform the shader does not implement. The `tone_map_op`
    /// encoding is 0 none, 1 Reinhard, 2 Hable, 3 the whole ACES Rec.709
    /// output transform; the caller has already established which of
    /// those its delivery is.
    ///
    /// `display_tables` is whichever baked table set `target` asks for —
    /// `klipp_color::aces::aces_rec709_gpu_data` for `tone_map_op == 3`,
    /// `klipp_color::gamut::bt2020_to_bt709_gpu_data` for `gamut_op == 1`
    /// — and empty when the transform needs none. Passed in rather than
    /// built here because this crate deliberately does not depend on
    /// `klipp-color` outside its tests.
    pub fn encode_chain_to_yuv420(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: Yuv420Target,
        display_tables: &[f32],
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_yuv420_inner(slot_a, width, height, target, display_tables)
            .map(|(bytes, _)| bytes)
    }

    /// [`Self::encode_chain_to_yuv420`], and the frame's light levels —
    /// what HDR10's MaxCLL and MaxFALL are made of.
    ///
    /// A PQ delivery needs them, and they used to be the reason it could
    /// not take this path at all: the twin measures light per pixel while
    /// it encodes, off the linear frame this path exists not to download.
    /// The shader measures the same thing where the pixels are, reducing
    /// to one `(max, sum)` a workgroup, and the host finishes the
    /// reduction over those few thousand pairs.
    ///
    /// `None` for the same reasons as the unmeasured form, and also when
    /// the target is not a PQ delivery — the shader writes no light on
    /// any other path, so a caller asking for it has the wrong one.
    pub fn encode_chain_to_yuv420_measured(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: Yuv420Target,
        display_tables: &[f32],
    ) -> Option<(Vec<u8>, GpuFrameLight)> {
        if target.hdr_op != 1 {
            return None;
        }
        let (bytes, light) =
            self.encode_chain_to_yuv420_inner(slot_a, width, height, target, display_tables)?;
        Some((bytes, light?))
    }

    pub(super) fn encode_chain_to_yuv420_inner(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: Yuv420Target,
        display_tables: &[f32],
    ) -> Option<(Vec<u8>, Option<GpuFrameLight>)> {
        let Yuv420Target {
            tone_map_op,
            // Names which ODT blob the caller built; nothing here reads
            // it, and the shader takes the transform from the blob.
            odt_tag: _,
            gamut_op,
            hdr_op,
            peak_nits,
            scene,
            luma_coefs,
            semi_planar,
            bits,
            sample_shift,
        } = target;
        let (scene_op, scene_kind, scene_scale, scene_exposure) = match scene {
            Some(s) => (if s.display_light { 2 } else { 1 }, s.kind, s.scale, s.exposure),
            None => (0, 0, 1.0, 1.0),
        };
        if self.is_device_lost()
            || !width.is_multiple_of(8)
            || !height.is_multiple_of(2)
            || tone_map_op > 3
            || gamut_op > 1
            || hdr_op > 2
            // All three read binding 3, so one transform at a time.
            || (gamut_op == 1 && tone_map_op == 3)
            || (hdr_op != 0 && (gamut_op == 1 || tone_map_op == 3))
            || !matches!(bits, 8 | 10)
        {
            return None;
        }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 {
            return None;
        }
        // Two bytes a sample at ten bits, one at eight.
        let sample_bytes = if bits == 8 { 1 } else { 2 };
        let luma_bytes = pixel_count * sample_bytes;
        let chroma_bytes = (pixel_count / 4) * sample_bytes;
        let total = luma_bytes + 2 * chroma_bytes;

        // Before `source` borrows self: uploading to the fourth binding
        // needs &mut, and every table set is cached for the export's life
        // because none changes frame to frame.
        if tone_map_op == 3 {
            if display_tables.is_empty() {
                return None;
            }
            self.ensure_aces_data(display_tables);
        } else if gamut_op == 1 && !self.ensure_bt2020_gamut_data(display_tables) {
            return None;
        } else if (hdr_op != 0 || scene_op == 2) && !self.ensure_hdr_encode_data(display_tables) {
            // The display-light branch reads the working space's luma
            // weights out of the same blob (the HLG OOTF needs them), so
            // it wants the blob even when the delivery is SDR.
            return None;
        }

        // One `(max, sum)` pair a workgroup, which only a PQ delivery
        // writes. The dispatch shape below has to match this exactly, so
        // both read the same expression.
        let (wg_x, wg_y) = ((width / 8).div_ceil(8), (height / 2).div_ceil(8));
        let measuring = hdr_op == 1;
        let light_floats = (wg_x as u64) * (wg_y as u64) * 2;
        let light_buf = measuring.then(|| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("yuv420_light"),
                size: light_floats * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        });

        let source = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let source = source?;
        // The planes need their own buffer: `buf_c` is a chain slot and
        // may hold a frame the graph still wants.
        let planes = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuv420_planes"),
            size: total,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        self.write_params(&Yuv420Params {
            width,
            height,
            tone_map_op,
            chroma_word_offset: (luma_bytes / 4) as u32,
            kr: luma_coefs[0],
            kg: luma_coefs[1],
            kb: luma_coefs[2],
            semi_planar: semi_planar as u32,
            bits,
            sample_shift,
            gamut_op,
            hdr_op,
            peak_nits,
            scene_op,
            scene_kind,
            scene_scale,
            scene_exposure,
            _pad2: 0,
            _pad3: 0,
        });
        let tables: &wgpu::Buffer = if tone_map_op == 3 {
            &self.aces_rec709_data.as_ref()?.buf
        } else if gamut_op == 1 {
            &self.bt2020_gamut_data.as_ref()?.buf
        } else if hdr_op != 0 || scene_op == 2 {
            &self.hdr_encode_data.as_ref()?.buf
        } else {
            &self.pipelines.no_display_tables
        };
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("linear_to_yuv420_bg"),
            layout: &self.pipelines.display_tables_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: source.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: planes.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: tables.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: light_buf
                        .as_ref()
                        .unwrap_or(&self.pipelines.no_light)
                        .as_entire_binding(),
                },
            ],
        });

        // Ride the open chain encoder when there is one, so the whole
        // frame is a single submit; NVIDIA's driver charges 5-15 ms for
        // each one.
        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuv420") })
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.linear_to_yuv420_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }
        self.submit(encoder);

        // `staging_read` is sized for a whole linear frame, which is ten
        // times these planes, so the chain having run means it fits.
        let bytes = self.with_downloaded(&planes, total, |bytes| bytes.to_vec())?;
        let light = match light_buf {
            // The per-workgroup pairs, finished on the CPU. Summed in f64
            // the way the twin sums its per-pixel nits, over a few
            // thousand partials rather than eight million pixels.
            Some(buf) => self.with_downloaded(&buf, light_floats * 4, |raw| {
                let pairs: &[f32] = bytemuck::cast_slice(&raw[..(light_floats as usize) * 4]);
                let mut max_nits = 0.0f32;
                let mut sum_nits = 0.0f64;
                for p in pairs.chunks_exact(2) {
                    max_nits = max_nits.max(p[0]);
                    sum_nits += p[1] as f64;
                }
                GpuFrameLight { max_nits, sum_nits, pixels: pixel_count }
            }),
            None => None,
        };
        Some((bytes, light))
    }

    /// The chain's graded frame → HDR monitoring's light map: the
    /// brightest channel of every `step`-th pixel, in cd/m². Row-major,
    /// `width.div_ceil(step)` by `height.div_ceil(step)`.
    ///
    /// `to_2020` is the working space's basis into BT.2020, packed by
    /// `color_wgsl::pack_primaries_matrix` — a delivery is BT.2020
    /// whatever the project works in, and measured in a narrower basis the
    /// same BT.2020 red reads 67% high.
    ///
    /// This is the piece that lets HDR monitoring keep the fast path: the
    /// readout needs the light, and the light needs the linear frame, so
    /// without it the frame has to come down. A quarter-resolution map is
    /// 2 MB at 4K against the frame's 132.
    pub fn nit_map_of_chain(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        step: u32,
        to_2020: [[f32; 4]; 3],
    ) -> Option<(Vec<f32>, u32, u32)> {
        if self.is_device_lost() || width == 0 || height == 0 {
            return None;
        }
        let step = step.max(1);
        let (mw, mh) = (width.div_ceil(step), height.div_ceil(step));
        let bytes = (mw as u64) * (mh as u64) * 4;
        if self.hdr_nit_buf.as_ref().is_none_or(|b| b.size() < bytes) {
            self.hdr_nit_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hdr_nit_map"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.hdr_nit_staging = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hdr_nit_staging"),
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        self.write_params(&NitMapParams { width, height, step, map_width: mw, to_2020 });
        let src = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.hdr_nit_buf.as_ref()?;
        let staging = self.hdr_nit_staging.as_ref()?;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hdr_nit_map_bg"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("hdr_nit_map") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.hdr_nit_map_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (mw * mh).div_ceil(WORKGROUP_SIZE));
        }
        let mut readback = self.readbacks.begin(bytes);
        enc.copy_buffer_to_buffer(out, 0, staging, 0, bytes);
        self.submit(enc);

        let slice = staging.slice(..bytes);
        self.flush();
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let map: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        staging.unmap();
        readback.succeeded = true;
        Some((map, mw, mh))
    }

    /// The chain's graded AcesCG frame → display-encoded Rec.709 SDR
    /// bytes, the ACES output transform run on the GPU.
    ///
    /// Replaces `klipp_color::aces::acescg_to_rec709_sdr` per pixel on
    /// the CPU, which is 88 ms of a 4K frame — the reason an AcesCG
    /// project exported six times slower than a Rec.709 one.
    ///
    /// `aces_data` is `klipp_color::aces::aces_rec709_gpu_data` — the
    /// matrices and tone curves. It is passed in rather than built here
    /// because this crate deliberately does not depend on
    /// `klipp-color` outside its tests: the colour science lives there,
    /// and this is the layer that runs things on a GPU. Uploaded once;
    /// the data is fixed for this ODT.
    ///
    /// `None` when there is no GPU or the device is lost; callers keep
    /// the CPU transform.
    pub fn encode_chain_to_aces_rec709_u8(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        aces_data: &[f32],
    ) -> Option<Vec<u8>> {
        if self.is_device_lost() {
            return None;
        }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 {
            return None;
        }
        self.ensure_aces_data(aces_data);
        let out_bytes = pixel_count * 4;

        let source = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aces_rec709_out"),
            size: out_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let data = &self.aces_rec709_data.as_ref()?.buf;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aces_rec709_bg"),
            layout: &self.pipelines.display_tables_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: source.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: data.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: self.pipelines.no_light.as_entire_binding() },
            ],
        });

        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("aces") })
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.aces_rec709_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (pixel_count as u32).div_ceil(WORKGROUP_SIZE));
        }
        self.submit(encoder);

        self.with_downloaded(&out, out_bytes, |bytes| bytes.to_vec())
    }

    /// Upload the ACES matrices and tone curves, once.
    pub(super) fn ensure_aces_data(&mut self, data: &[f32]) {
        Self::ensure_tables(
            &self.device,
            &self.queue,
            &mut self.aces_rec709_data,
            "aces_odt_data",
            data,
        );
    }

    /// Upload `data` to `slot`, reusing what is there when the contents
    /// are the same.
    ///
    /// Keyed on the contents and not merely on "is something uploaded",
    /// which is how it started: an output transform's numbers are fixed
    /// for a *deliverable*, not for a pipeline, and the four ACES ODTs
    /// share one slot. Skipping the second upload rendered the second
    /// deliverable with the first one's matrices and gammas — the same
    /// picture, silently, which is how
    /// `every_acescg_odt_matches_the_cpu` found this. Comparing a few
    /// thousand floats is nothing beside a frame.
    pub(super) fn ensure_tables(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: &mut Option<CachedTables>,
        label: &'static str,
        data: &[f32],
    ) {
        if let Some(c) = slot {
            if c.data == data {
                return;
            }
        }
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (data.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buf, 0, bytemuck::cast_slice(data));
        *slot = Some(CachedTables { buf, data: data.to_vec() });
    }

    /// Upload `klipp_color::gamut::bt2020_to_bt709_gpu_data` once. The
    /// tables are fixed for this gamut pair, so later calls are free.
    ///
    /// `false` for a buffer the shader would read past the end of. The
    /// WGSL takes its grid size from the first two entries and indexes
    /// the reach and cusp tables off it, so a short or truncated buffer
    /// is not merely wrong pixels — the length the header implies is
    /// recomputed here, the one place the host can check it. That header
    /// arithmetic mirrors `gamut::gpu_layout`, which
    /// `gpu_gamut_data_length_is_what_the_header_says` in `klipp-color`
    /// holds the two to.
    pub(super) fn ensure_bt2020_gamut_data(&mut self, data: &[f32]) -> bool {
        // HEADER_LEN 2 + MATRICES_LEN 27 + WHITE_LEN 3 = 32 before the
        // first table; then two reach tables and the cusps.
        let (l_steps, h_steps) = match (data.first(), data.get(1)) {
            (Some(&l), Some(&h)) if l >= 1.0 && h >= 1.0 && l < 4096.0 && h < 4096.0 => {
                (l as usize, h as usize)
            }
            _ => return false,
        };
        let reach = l_steps * h_steps + l_steps;
        if data.len() < 32 + 2 * reach + h_steps * 2 {
            return false;
        }
        Self::ensure_tables(
            &self.device,
            &self.queue,
            &mut self.bt2020_gamut_data,
            "bt2020_gamut_data",
            data,
        );
        true
    }

    /// Upload `klipp_color::hdr::hdr_encode_gpu_data` once: the basis
    /// matrix, the working space's luma weights and the PQ table.
    ///
    /// `false` for a buffer the shader would read past the end of. The
    /// WGSL takes the PQ table's length from the first entry and indexes
    /// off it, so the length that header implies is checked here — the
    /// one place the host can. `gpu_hdr_data_length_is_what_the_header_says`
    /// in `klipp-color` holds the two layouts together, since this crate
    /// cannot see `hdr::gpu_layout`.
    pub(super) fn ensure_hdr_encode_data(&mut self, data: &[f32]) -> bool {
        // 1 header + 9 matrix + 3 luma = 13 before the table.
        let entries = match data.first() {
            Some(&n) if (2.0..65536.0).contains(&n) => n as usize,
            _ => return false,
        };
        if data.len() < 13 + entries {
            return false;
        }
        Self::ensure_tables(
            &self.device,
            &self.queue,
            &mut self.hdr_encode_data,
            "hdr_encode_data",
            data,
        );
        true
    }

    /// The chain's graded BT.2020 frame → display bytes: the basis
    /// change, the BT.2407-style gamut compression and the sRGB encode.
    ///
    /// Replaces `display_transform_ws(.., Bt2020Linear)` on the CPU: 51 ms
    /// of a saturated 4K frame, plus 14 ms to bring the floats down, for
    /// 4.1 ms here. No tone-map curve: the caller only takes this with
    /// `ToneMapOp::None`, as it does for BT.709.
    ///
    /// `gamut_data` is `klipp_color::gamut::bt2020_to_bt709_gpu_data`,
    /// passed in for the same reason the ACES data is — this crate does
    /// not depend on `klipp-color` outside its tests.
    pub fn encode_chain_to_bt2020_display_u8(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        gamut_data: &[f32],
    ) -> Option<Vec<u8>> {
        if self.is_device_lost() {
            return None;
        }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 || !self.ensure_bt2020_gamut_data(gamut_data) {
            return None;
        }

        let out_bytes = pixel_count * 4;
        let source = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bt2020_display_out"),
            size: out_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let data = &self.bt2020_gamut_data.as_ref()?.buf;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bt2020_display_bg"),
            layout: &self.pipelines.display_tables_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: source.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: data.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: self.pipelines.no_light.as_entire_binding() },
            ],
        });

        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("bt2020") })
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.bt2020_display_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (pixel_count as u32).div_ceil(WORKGROUP_SIZE));
        }
        self.submit(encoder);

        self.with_downloaded(&out, out_bytes, |bytes| bytes.to_vec())
    }

    /// Put a frame that is already linear working-space f32 on the GPU
    /// into the chain's input slot. A buffer copy, not a conversion —
    /// there is nothing to expand.
    ///
    /// GPU-local, so it is not the kind of copy the bridges exist to
    /// avoid: 133 MB at 4K moves in well under a millisecond on device
    /// memory, against ~10 ms for the same bytes over PCIe.
    ///
    /// `false` when the device is lost or `input` is too small; callers
    /// fall back to handing the pixels over on the CPU.
    pub fn copy_linear_into_chain(
        &mut self,
        input: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        let byte_size = pixel_count * 16;
        self.ensure_buffers(byte_size);
        if input.size() < byte_size { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        let mut encoder = self.encoder(Some("copy_linear_into_chain"));
        encoder.copy_buffer_to_buffer(input, 0, buf_a, 0, byte_size);
        self.submit(encoder);
        true
    }

    /// [`Self::bridge_u8_to_linear_into_chain`] one sample wider:
    /// sRGB-encoded RGBA64 → linear working-space f32 in `buf_a`, the
    /// source primaries folded in.
    ///
    /// This is the export's input transform. Its scaler produces RGBA64
    /// even from an 8-bit source — YUV→RGB is a matrix, and its output
    /// genuinely needs more than 8 bits — so narrowing on the way here
    /// would quantise the delivered frame. Instead the scaler's own
    /// buffer is uploaded as it stands and expanded in place: half the
    /// bus traffic of the linear f32 buffer it replaces, and none of the
    /// per-pixel CPU pass that used to build it.
    ///
    /// `false` when the device is lost or `input` is too small; callers
    /// fall back to expanding on the CPU.
    pub fn bridge_rgba64_to_linear_into_chain(
        &mut self,
        input_rgba64: &wgpu::Buffer,
        width: u32,
        height: u32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        self.ensure_buffers(pixel_count * 16);

        // Four u16 per pixel — twice the u8 bridge's input stride.
        if input_rgba64.size() < pixel_count * 8 { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&Rgba64BridgeParams {
            width,
            height,
            _pad0: 0,
            _pad1: 0,
            source_to_working,
        });

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_rgba64_to_linear_bg"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_rgba64.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.pipelines.srgb16_lut.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_rgba64_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.pipelines.bridge_rgba64_to_linear_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        true
    }

    /// GPU input transform for HDR: packed RGB10 code values (the P010
    /// decoder's `hdr_codes`) → scene-linear working-space f32 in `buf_a`,
    /// leaving the chain primed exactly as `bridge_u8_to_linear_into_chain`
    /// does for SDR. `transfer_id` is a `klipp_core::color_ids` PQ/HLG id.
    ///
    /// Returns `false` when the device is lost or `codes` is too small;
    /// callers fall back to the CPU input transform.
    pub fn bridge_hdr10_to_linear_into_chain(
        &mut self,
        codes: &wgpu::Buffer,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        self.bridge_scene_codes_into_chain(codes, false, width, height, transfer_id, scene_scale, source_to_working)
    }

    /// [`Self::bridge_hdr10_to_linear_into_chain`] for 16-bit codes
    /// (`klipp_color::hdr::pack_rgb16`, eight bytes a pixel): a source
    /// deeper than 10 bits, at its own precision (#1226).
    pub fn bridge_hdr16_to_linear_into_chain(
        &mut self,
        codes: &wgpu::Buffer,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        self.bridge_scene_codes_into_chain(codes, true, width, height, transfer_id, scene_scale, source_to_working)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn bridge_scene_codes_into_chain(
        &mut self,
        codes: &wgpu::Buffer,
        deep: bool,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        self.ensure_buffers(pixel_count * 16);
        if codes.size() < pixel_count * if deep { 8 } else { 4 } { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&Hdr10BridgeParams {
            width, height, transfer_id, scene_scale, source_to_working,
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_hdr10_to_linear_bg"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: codes.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut encoder = self.encoder(Some("bridge_hdr10_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(if deep { &self.pipelines.bridge_hdr16_pipeline } else { &self.pipelines.bridge_hdr10_pipeline });
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (pixel_count as u32).div_ceil(WORKGROUP_SIZE));
        }
        self.submit(encoder);
        true
    }

    /// Encode the chain's final linear output (`buf_a` when `slot_a`,
    /// else `buf_b`) to display sRGB u8 on the GPU and read back only
    /// that: 4 bytes a pixel instead of the 16 a linear download costs —
    /// 33 MB at 4K rather than 132. `hdr_tonemap` applies the HDR display
    /// tone map first, making this an HDR clip's last step.
    ///
    /// Only valid where the CPU display path would be
    /// `display_transform_ws(ToneMapOp::None, Rec709/BT2020 linear)`; the
    /// caller gates on that. `None` on device loss or a failed readback.
    pub fn encode_chain_to_display_u8(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_display_u8_view(slot_a, width, height, tonemap_exposure, false)
    }

    /// [`Self::encode_chain_to_display_u8`], and for `hdr_preview` the
    /// frame is HDR display light rather than SDR linear: the light above
    /// reference white rolls into the top so its structure survives a
    /// screen that cannot show it.
    ///
    /// That is HDR monitoring's `Preview` mode, and it exists here so
    /// turning monitoring on does not cost the frame its place on the GPU.
    /// `klipp_color::hdr_view::preview_frame` is the twin.
    pub fn encode_chain_to_display_u8_view(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
        hdr_preview: bool,
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_display_u8_monitor(
            slot_a, width, height, tonemap_exposure, hdr_preview, None,
        )
    }

    /// [`Self::encode_chain_to_display_u8_view`], and for a
    /// scene-referred clip the display-light pass it ends with — which is
    /// what HDR monitoring wants to see rather than the SDR tone map.
    ///
    /// `scene` is `(HdrTransfer::display_light_kind, scene_scale,
    /// peak_nits, working luma weights)`; `None` for a display-referred
    /// clip, which has no such pass.
    pub fn encode_chain_to_display_u8_monitor(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
        hdr_preview: bool,
        scene: Option<(u32, f32, f32, [f32; 3])>,
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_display(slot_a, width, height, tonemap_exposure, hdr_preview, scene, None, true)
            .map(Option::unwrap_or_default)
    }

    /// The display encode of [`Self::encode_chain_to_display_u8_monitor`],
    /// delivered where the caller wants it: into `target` — an
    /// `Rgba8Unorm` texture of the frame's size with `STORAGE_BINDING`,
    /// the viewer's own — and, when `want_bytes`, to the CPU as well.
    /// Neither costs the other anything but the texel store; with no
    /// bytes wanted the frame never leaves the GPU, which is the point.
    ///
    /// `None` on a lost device or a target of the wrong shape; otherwise
    /// the bytes when they were wanted.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_chain_to_display(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
        hdr_preview: bool,
        scene: Option<(u32, f32, f32, [f32; 3])>,
        target: Option<&wgpu::Texture>,
        want_bytes: bool,
    ) -> Option<Option<Vec<u8>>> {
        if self.is_device_lost() { return None; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return Some(want_bytes.then(Vec::new)); }
        if let Some(t) = target {
            let size = t.size();
            if size.width != width || size.height != height || t.format() != wgpu::TextureFormat::Rgba8Unorm
                || !t.usage().contains(wgpu::TextureUsages::STORAGE_BINDING)
            {
                tracing::warn!(
                    "encode_chain_to_display: target {}x{} {:?} {:?} cannot take a {width}x{height} frame",
                    size.width, size.height, t.format(), t.usage(),
                );
                return None;
            }
        }
        let out_bytes = pixel_count * 4;
        if self.bridge_u8_out_size < out_bytes {
            self.bridge_u8_out_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_out"),
                size: out_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.bridge_u8_out_size = out_bytes;
        }
        if self.display_u8_staging.as_ref().is_none_or(|b| b.size() < out_bytes) {
            self.display_u8_staging = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("display_u8_staging"),
                size: out_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let src = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.bridge_u8_out_buf.as_ref().unwrap();
        let staging = self.display_u8_staging.as_ref().unwrap();
        self.write_params(&DisplayU8Params {
            width,
            height,
            hdr_tonemap: tonemap_exposure.is_some() as u32,
            exposure: tonemap_exposure.unwrap_or(0.0),
            view_op: hdr_preview as u32,
            // 3 means "no display-light pass"; the kinds are 0, 1, 2.
            scene_kind: scene.map_or(3, |s| s.0),
            scene_scale: scene.map_or(1.0, |s| s.1),
            peak_nits: scene.map_or(0.0, |s| s.2),
            luma: scene.map_or([0.2126, 0.7152, 0.0722, 0.0], |s| {
                [s.3[0], s.3[1], s.3[2], 0.0]
            }),
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("linear_to_display_u8_bg"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut encoder = self.encoder(Some("linear_to_display_u8"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.pipelines.display_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (pixel_count as u32).div_ceil(WORKGROUP_SIZE));
        }
        if let Some(t) = target {
            // Its own params buffer: the shared one is read by the encode
            // pass ahead of this in the same encoder.
            let view = t.create_view(&wgpu::TextureViewDescriptor::default());
            let tex_params = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("u8_to_texture_params"),
                size: std::mem::size_of::<BridgeU8Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&tex_params, 0, bytemuck::bytes_of(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 }));
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("u8_to_texture_bg"),
                layout: &self.pipelines.u8_to_texture_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: out.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: tex_params.as_entire_binding() },
                ],
            });
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: Some("u8_to_texture"), timestamp_writes: None },
            );
            pass.set_pipeline(&self.pipelines.u8_to_texture_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
        }
        let mut readback = want_bytes.then(|| self.readbacks.begin(out_bytes));
        if want_bytes {
            encoder.copy_buffer_to_buffer(out, 0, staging, 0, out_bytes);
        }
        self.submit(encoder);
        // The frame is encoded: everything recorded goes to the queue.
        self.stack_end();
        if !want_bytes {
            return Some(None);
        }

        let slice = staging.slice(..out_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        self.flush();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let bytes = slice.get_mapped_range().to_vec();
                staging.unmap();
                if let Some(attempt) = &mut readback { attempt.succeeded = true; }
                Some(Some(bytes))
            }
            other => {
                self.mark_device_lost(&format!("display_u8 map_async failed: {other:?}"));
                None
            }
        }
    }

    /// Dispatch-only display step of a scene-referred clip (phase 2b M2,
    /// step 11c): `klipp_color::hdr::SceneToDisplay::apply` on the chain,
    /// so the effects that follow take the picture where it sits. `kind`,
    /// `scene_scale` and `exposure` are the source transfer's
    /// (`display_light_kind`, `scene_scale`, `display_exposure`); a `peak`
    /// is display light, none the SDR tone map; `luma` is the working
    /// space's luminance weights.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_scene_to_display_chain(&mut self, in_is_a: bool, width: u32, height: u32, kind: u32, scene_scale: f32, exposure: f32, peak: Option<f32>, luma: [f32; 3]) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuSceneToDisplayParams {
            width,
            height,
            light: u32::from(peak.is_some()),
            kind,
            scene_scale,
            exposure,
            peak: peak.unwrap_or(0.0),
            _pad: 0,
            luma_r: luma[0],
            luma_g: luma[1],
            luma_b: luma[2],
            _pad1: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.scene_to_display_pipeline, bind_group, workgroups);
    }

    /// Upload an sRGB u8 RGBA frame to a pixel storage buffer as
    /// native linear-f32 vec4s (16 bytes/pixel). Callers pass `Vec<u8>`
    /// at the boundary; this method handles the sRGB→linear decode +
    /// GPU upload in one shot.
    pub(super) fn upload_u8_rgba_as_linear(&self, buf: &wgpu::Buffer, rgba: &[u8]) {
        let linear = u8_rgba_to_linear_f32(rgba);
        self.queue
            .write_buffer(buf, 0, bytemuck::cast_slice(&linear));
    }

    /// Download `pixel_count` linear-f32 RGBA vec4s from a pixel
    /// storage buffer and convert to sRGB u8 RGBA at the boundary.
    /// Retained for the u8-boundary paths (scopes, transition helpers,
    /// 3D LUT) that still speak u8.
    #[allow(dead_code)]
    pub(super) fn download_linear_rgba_as_u8(
        &self,
        source: &wgpu::Buffer,
        pixel_count: usize,
    ) -> Vec<u8> {
        let byte_size = (pixel_count * 16) as u64;
        let bytes = self.download(source, byte_size);
        if bytes.len() < pixel_count * 16 {
            // Device-lost fallback: `download` returned zero bytes.
            return vec![0u8; pixel_count * 4];
        }
        let linear: &[f32] = bytemuck::cast_slice(&bytes);
        linear_f32_rgba_to_u8(linear)
    }

    /// Same as [`download_linear_rgba_as_u8`] but treats the buffer
    /// as already-perceptual / alpha data — no sRGB encoding applied
    /// on the way out. Used by mask matte downloads where the
    /// shader writes a 0..1 coverage value (not a linear-light
    /// luminance) to all RGB channels and the consumer (the test
    /// harness, an image viewer, the CPU compositor) expects that
    /// value to land at byte position `(matte * 255 + 0.5) as u8`
    /// without any gamma curve. Without this path, mask matte 0.5
    /// gets gamma-encoded to ~187/255 instead of the expected
    /// 128/255 — a 60-LSB drift that hid in production until the
    /// CPU-vs-GPU parity test caught it.
    pub(super) fn download_raw_rgba_as_u8(
        &self,
        source: &wgpu::Buffer,
        pixel_count: usize,
    ) -> Vec<u8> {
        let byte_size = (pixel_count * 16) as u64;
        let bytes = self.download(source, byte_size);
        if bytes.len() < pixel_count * 16 {
            return vec![0u8; pixel_count * 4];
        }
        let f32s: &[f32] = bytemuck::cast_slice(&bytes);
        let mut out = Vec::with_capacity(pixel_count * 4);
        for chunk in f32s.chunks_exact(4) {
            out.push((chunk[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            out.push((chunk[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            out.push((chunk[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            out.push((chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
        out
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BridgeU8Params {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BridgeU8FitParams {
    pub(super) src_w: u32,
    pub(super) src_h: u32,
    pub(super) out_w: u32,
    pub(super) out_h: u32,
    pub(super) dst_w: u32,
    pub(super) dst_h: u32,
    pub(super) x_off: u32,
    pub(super) y_off: u32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Yuv420Params {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tone_map_op: u32,
    pub(super) chroma_word_offset: u32,
    pub(super) kr: f32,
    pub(super) kg: f32,
    pub(super) kb: f32,
    pub(super) semi_planar: u32,
    pub(super) bits: u32,
    pub(super) sample_shift: u32,
    pub(super) gamut_op: u32,
    pub(super) hdr_op: u32,
    pub(super) peak_nits: f32,
    pub(super) scene_op: u32,
    pub(super) scene_kind: u32,
    pub(super) scene_scale: f32,
    pub(super) scene_exposure: f32,
    pub(super) _pad2: u32,
    pub(super) _pad3: u32,
}

/// A table set uploaded to the display-transform binding, with the
/// floats it was built from so a different set replaces it.
pub(super) struct CachedTables {
    pub(super) buf: wgpu::Buffer,
    pub(super) data: Vec<f32>,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Rgba64BridgeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
    /// Source primaries → working space, 3 rows of vec4 (w unused).
    pub(super) source_to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Hdr10BridgeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) transfer_id: u32,
    /// Curve output -> scene-linear: 100 for PQ/HLG, 1 for camera logs.
    pub(super) scene_scale: f32,
    /// Source primaries → working space, 3 rows of vec4 (w unused).
    pub(super) source_to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct DisplayU8Params {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) hdr_tonemap: u32,
    /// Exposure ahead of the tone map (`HdrTransfer::display_exposure`).
    pub(super) exposure: f32,
    /// 0 the ordinary SDR encode, 1 HDR monitoring's preview roll-off.
    pub(super) view_op: u32,
    /// `HdrTransfer::display_light_kind` for a scene-referred clip, whose
    /// own last step runs before the preview roll-off; 3 or more for a
    /// display-referred one, which has none.
    pub(super) scene_kind: u32,
    pub(super) scene_scale: f32,
    pub(super) peak_nits: f32,
    /// The working space's luma weights, for the HLG branch of the
    /// display-light pass. `w` is padding: a uniform `vec4` needs it.
    pub(super) luma: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct NitMapParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) step: u32,
    pub(super) map_width: u32,
    pub(super) to_2020: [[f32; 4]; 3],
}

/// [`shader_scene_to_display`]'s uniform: `klipp_color::hdr::SceneToDisplay`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuSceneToDisplayParams {
    pub(super) width: u32, pub(super) height: u32,
    /// 1 for display light (a peak is set), 0 for the SDR tone map.
    pub(super) light: u32,
    /// `HdrTransfer::display_light_kind`.
    pub(super) kind: u32,
    pub(super) scene_scale: f32, pub(super) exposure: f32, pub(super) peak: f32, pub(super) _pad: u32,
    pub(super) luma_r: f32, pub(super) luma_g: f32, pub(super) luma_b: f32, pub(super) _pad1: u32,
}

/// HDR monitoring's light measurement: display light → the brightest
/// channel's level in cd/m², on a coarse grid.
///
/// `klipp_color::hdr_view::HdrMeasure::nit_map`'s twin. The brightest
/// *channel* and not the luminance, measured in BT.2020 whatever the
/// project works in — a saturated highlight runs out of range channel by
/// channel, and measured in a narrower basis the same BT.2020 red reads
/// 67% high. The matrix comes in as data for that reason.
///
/// Coarse on purpose: the readout under the pointer does not need a value
/// per pixel, and a whole-frame map is 33 MB at 4K where a
/// quarter-resolution one is 2.
pub(super) const SHADER_HDR_NIT_MAP: &str = gpu_shader!("output/hdr_nit_map.wgsl");

/// A scene-referred clip's display step as a chain pass (phase 2b M2, step
/// 11c): `klipp_color::hdr::SceneToDisplay::apply`, the SDR tone map or
/// display light, from the two shared definitions the decoders' previews
/// and the export's writer use. Signed like the CPU: unlike the viewer's
/// monitor encode it doesn't clamp first, since effects still follow.
pub(super) const SHADER_SCENE_TO_DISPLAY_BODY: &str = gpu_shader!("output/scene_to_display_body.wgsl");

// Linear-f32 RGBA → sRGB-u8 RGBA encode. One workgroup-thread per
// output pixel. Output buffer is `array<u32>` where each u32 packs
// RGBA bytes in little-endian order (R = byte 0, A = byte 3),
// matching the `Vec<u8>` format the rest of the engine consumes.
//
// Math is the standard sRGB transfer function (12.92 / 1.055·x^(1/2.4)
// with the linear toe at 0.0031308). Bit-identical to the CPU
// `linear_to_srgb` reference within rounding to u8. The CPU LUT we
// replace was also a quantised approximation (4096 input bins → u8
// output) so the GPU result here is at least as accurate.
// Packed RGB10 HDR code values (source primaries, PQ/HLG-encoded, as the
// P010 decoder writes them) → scene-linear working-space f32 RGBA in the
// chain's input slot. Concatenated after the colourspace prelude, which
// supplies `display_to_linear` and the TRANSFER_* ids. Must match
// `klipp_color::hdr::hdr10_codes_to_scene_linear_rgba`: curve first, then
// the gamut matrix, 1.0 = 100 nits.
pub(super) const SHADER_HDR10_TO_LINEAR_BODY: &str = gpu_shader!("output/hdr10_to_linear_body.wgsl");

// SHADER_HDR10_TO_LINEAR_BODY for 16-bit codes, a source deeper than 10
// bits (#1226): `klipp_color::hdr::pack_rgb16`, R and G in a pixel's
// first word, B in the low half of its second. Must match
// `klipp_color::hdr::hdr16_codes_to_scene_linear_rgba`.
pub(super) const SHADER_HDR16_TO_LINEAR_BODY: &str = gpu_shader!("output/hdr16_to_linear_body.wgsl");

// Chain output (linear f32 RGBA) → display sRGB u8, for a graded frame
// that nothing else needs on the CPU. With `hdr_tonemap` set it applies
// the HDR display tone map first — the GPU half of
// `klipp_color::hdr::apply_hdr_display_tonemap` — so an HDR clip's last
// step runs here rather than on a 132 MB CPU copy. Without it this is the
// SDR display encode (`ToneMapOp::None`: clamp, sRGB). Same layout as
// SHADER_LINEAR_TO_U8.
pub(super) const SHADER_LINEAR_TO_DISPLAY_U8: &str = gpu_shader!("output/linear_to_display_u8.wgsl");

pub(super) const SHADER_LINEAR_TO_U8: &str = gpu_shader!("output/linear_to_u8.wgsl");

pub(super) const SHADER_U8_TO_LINEAR: &str = gpu_shader!("output/u8_to_linear.wgsl");

// A frame of another size fitted into the chain's, as the CPU
// compositor's `fit` does: outside the picture transparent; inside, an
// area average of the source pixels an output pixel covers when the
// source is at least twice the size both ways, bilinear otherwise, on
// linear values with straight alpha.
pub(super) const SHADER_U8_FIT_TO_LINEAR: &str = gpu_shader!("output/u8_fit_to_linear.wgsl");

// A BT.2020 project's viewer display transform, on the GPU.
//
// `display_transform_ws(.., Bt2020Linear)` is the basis change, the
// BT.2407-style gamut compression, then the sRGB encode — ~15 ms a 4K
// frame for ordinary colour, ~32 for wholly saturated. The viewer's
// fast path declined the whole project for want of this.
//
// No tone-map curve here: the fast path only engages with
// `ToneMapOp::None`, exactly as it does for BT.709, where the operator
// would otherwise sit between the mapping and the encode. With one
// active the frame takes the CPU transform.
// `shader_linear_helpers_wgsl` already carries the pixel storage hook.
pub(super) const SHADER_BT2020_DISPLAY_U8: &str = gpu_shader!("output/bt2020_display_u8.wgsl");

pub(super) const SHADER_ACES_REC709_U8: &str = gpu_shader!("output/aces_rec709_u8.wgsl");

// The end of the export: a graded linear frame straight to the encoder's
// Y'CbCr planes, tone map, display encode, colour matrix and chroma
// subsampling all in the one pass.
//
// This is what stops 132 MB crossing the bus. The frame used to come back
// as linear f32 so the CPU could apply the display transform and convert
// to YUV; doing both here means only the planes come down — 12.4 MB for
// 4:2:0 at 4K, a tenth of the traffic — and the per-pixel CPU pass is
// gone with it.
//
// It must agree with `encode_linear_direct` exactly, which is the CPU
// twin and the authority; `gpu_yuv_matches_cpu` holds them to a level.
// That means the same order of operations: negatives clamped, tone map,
// sRGB encode, clamp to [0,1], luma from the *display-encoded* RGB, and
// chroma from the mean of the display-encoded RGB over the block — not
// the mean of the chroma, which is a different number.
//
// One invocation per 8x2 pixel block, because WGSL stores `u32` and not
// bytes: eight pixels wide is two whole words of luma per row, and the
// four chroma samples they reduce to are one whole word each of Cb and
// Cr. Every write is word-aligned with no read-modify-write and no
// atomics. The caller guarantees width % 8 == 0 and height % 2 == 0.
// `shader_linear_helpers_wgsl` already carries the pixel storage hook.
pub(super) const SHADER_LINEAR_TO_YUV420: &str = gpu_shader!("output/linear_to_yuv420.wgsl");

// The same bridge one sample wider: sRGB-encoded RGBA64 (four u16 per
// pixel, two per u32) → linear f32, with the source primaries folded in.
//
// This is the export's input transform. A software decoder's scaler
// hands back RGBA64 even for an 8-bit source, because YUV→RGB is a
// matrix and its output does not fit in 8 bits — measured on real
// footage, only 27% of samples come back byte-replicated. So the bytes
// cannot be narrowed on the way to the GPU without quantising the
// deliverable; they can only be *moved* there unexpanded, which is what
// this does. Half the traffic of the f32 buffer it replaces, and the
// per-pixel CPU pass that built it disappears.
//
// `source_to_working` is applied in linear light after the curve, so a
// project whose working space is not the source's still takes this path
// — the matrix rows are the identity when there is nothing to change.
pub(super) const SHADER_RGBA64_TO_LINEAR: &str = gpu_shader!("output/rgba64_to_linear.wgsl");
