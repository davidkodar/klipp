//! The effect chain on the GPU (#2890): a clip's effects run as one
//! recorded chain of passes over two ping-pong buffers, its params ring,
//! the uploads in and downloads out, and the breaks where a pass needs the
//! CPU.

use super::*;

impl GpuEffectsPipeline {
    /// Pick (input, output) buffer refs from the current ping-pong slot.
    pub(super) fn chain_in_out(&self, in_is_a: bool) -> (&wgpu::Buffer, &wgpu::Buffer) {
        let buf_a = self.buf_a.as_ref().expect("ensure_chain_buffers must be called first");
        let buf_b = self.buf_b.as_ref().expect("ensure_chain_buffers must be called first");
        if in_is_a { (buf_a, buf_b) } else { (buf_b, buf_a) }
    }

    /// Start a new chain segment. Resets the slot ring and opens a fresh
    /// command encoder. If an encoder is already open (should not happen
    /// — executor pairs begin/submit in lock-step) we submit+drop it
    /// first so no recorded work is lost.
    pub fn begin_chain(&mut self) {
        if let Some(enc) = self.chain_encoder.take() {
            // Recover from a missed submit_chain. Should be unreachable
            // in well-formed callers.
            self.submit(enc);
        }
        self.chain_params.reset();
        self.plate_read_in_chain = false;
        // A stack frame's passes so far go first: the chain reads what
        // they wrote, and its own ring starts over.
        self.flush();
        let enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("chain_encoder") });
        self.chain_encoder = Some(enc);
    }

    /// Finalise the current chain segment: submit the batched encoder if
    /// one is live. No-op when called without a prior `begin_chain`
    /// (covers short-circuit / device-lost paths).
    pub fn submit_chain(&mut self) {
        if let Some(enc) = self.chain_encoder.take() {
            self.submit(enc);
            self.plate_read_in_chain = false;
            // Effects recorded into the chain (`chain_run_effects`) read
            // their uniforms from these slots; submitted, they are free.
            if let Some(slots) = self.effect_params.as_mut() {
                slots.reset();
            }
            // A slot written from the CPU was held for this encoder's readers.
            self.slot_pool.submitted();
            // Non-blocking nudge to the driver to reclaim deferred
            // resources (command buffers, staging allocations) between
            // chain segments. On NVIDIA Vulkan these otherwise
            // accumulate across long sessions — paired with the
            // pre-allocated bind group pools above, this is the second
            // belt against the observed `device lost (Dropped)` crash.
            // `Maintain::Poll` returns immediately — it's a hint, not a
            // fence.
            self.device.poll(wgpu::Maintain::Poll);
        }
    }

    /// Claim the next ring slot for a chain dispatch. Panics if the
    /// ring is exhausted — a 32-slot ring covers every realistic grade
    /// (full legacy chain is 13 nodes; multi-pass nodes claim 1 slot
    /// per pass). If this ever fires in the wild, bump
    /// `CHAIN_PARAMS_RING_SIZE` rather than masking with wraparound.
    pub(super) fn next_params_slot(&mut self) -> usize {
        self.chain_params.claim_index().unwrap_or_else(|| {
            panic!("chain params ring exhausted ({CHAIN_PARAMS_RING_SIZE} slots); bump CHAIN_PARAMS_RING_SIZE")
        })
    }

    /// Write `data` to the params-ring slot at `slot_idx`. Paired with
    /// `next_params_slot` by every `*_chain` entry.
    pub(super) fn write_chain_params<T: bytemuck::Pod>(&self, slot_idx: usize, data: &T) {
        let bytes = bytemuck::bytes_of(data);
        self.queue.write_buffer(self.chain_params.buffer(slot_idx), 0, bytes);
    }

    /// Record a single compute dispatch into the supplied encoder.
    /// Pulled out so each `*_chain` entry doesn't duplicate the
    /// `begin_compute_pass` boilerplate. Takes the encoder separately
    /// from `&self` so callers can hold an immutable borrow on other
    /// fields (pipelines, LUT buffers) while recording.
    pub(super) fn record_compute_dispatch(
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        bind_group: &wgpu::BindGroup,
        workgroups: u32,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        dispatch_flat(&mut pass, workgroups);
    }

    /// Buffer-pair configuration indexing for the pre-allocated chain
    /// bind group pools. The 3-binding pool stores all four variants
    /// (AtoB=0, BtoA=1, AtoC=2, BtoC=3); the 4-binding pools only need
    /// AtoB=0 / BtoA=1.
    #[inline]
    pub(super) fn chain_cfg_3b_ab(in_is_a: bool) -> usize { if in_is_a { 0 } else { 1 } }

    #[inline]
    pub(super) fn chain_cfg_3b_ac(in_is_a: bool) -> usize { if in_is_a { 2 } else { 3 } }

    #[inline]
    pub(super) fn chain_cfg_4b(in_is_a: bool) -> usize { if in_is_a { 0 } else { 1 } }

    /// A slowed clip's in-between frame on the chain by optical flow
    /// (#2854): from the chain's picture, or `from` in its place, toward
    /// `toward` (linear RGBA,
    /// `width` × `height`, the media's next frame on the GPU) at `t`, the
    /// pair `key` names estimated once and kept. `None` when it couldn't
    /// be recorded; else the share of the pair that couldn't be followed,
    /// once it is known (the draw cross-fades such a frame itself).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_flow_chain(&mut self, in_is_a: bool, width: u32, height: u32, from: Option<&wgpu::Buffer>, toward: &wgpu::Buffer, key: FlowKey, t: f32, luma: [f32; 3]) -> Option<Option<f32>> {
        let (a, b) = (self.buf_a.take(), self.buf_b.take());
        let made = match (&a, &b) {
            (Some(a), Some(b)) if in_is_a => self.record_flow_on_chain(key, from.unwrap_or(a), toward, b, width, height, t, luma),
            (Some(a), Some(b)) => self.record_flow_on_chain(key, from.unwrap_or(b), toward, a, width, height, t, luma),
            _ => None,
        };
        (self.buf_a, self.buf_b) = (a, b);
        made
    }

    /// Resize `buf_a` / `buf_b` / `buf_c` to hold at least `byte_size` bytes.
    /// Public entry point for the node executor's GPU-resident chain path.
    pub fn ensure_chain_buffers(&mut self, byte_size: u64) {
        self.ensure_buffers(byte_size);
    }

    /// Upload a linear-f32 RGBA frame into `buf_a`. Used by the node
    /// executor to seed the ping-pong chain.
    pub fn upload_chain_input_linear(&mut self, linear_rgba: &[f32]) {
        if self.is_device_lost() {
            return;
        }
        let buf_a = self.buf_a.as_ref().expect("ensure_chain_buffers must be called first");
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
    }

    /// Download the final linear-f32 RGBA frame. `slot_a` selects `buf_a`
    /// when true, `buf_b` when false — the caller's ping-pong parity.
    /// Read `matte_buf` back to the CPU.
    ///
    /// The slow way to get at a matte, and only correct once the
    /// dispatch that wrote it has been submitted — after a chain flush,
    /// in other words. It exists so a matte left in GPU memory by a node
    /// whose consumer then fell off the chain can still be blended
    /// correctly: the alternative is a silently un-applied secondary,
    /// which is a wrong picture rather than a slow one.
    pub fn read_matte_buf_linear(&self, pixel_count: usize) -> Vec<f32> {
        if self.is_device_lost() {
            return Vec::new();
        }
        match self.matte_buf.as_ref() {
            Some(buf) => self.download_linear_rgba_f32(buf, pixel_count),
            None => Vec::new(),
        }
    }

    pub fn download_chain_output_linear(&self, slot_a: bool, pixel_count: usize) -> Vec<f32> {
        let buf = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let buf = buf.expect("ensure_chain_buffers must be called first");
        self.download_linear_rgba_f32(buf, pixel_count)
    }

    /// Take ownership of the open chain encoder (if any) and finish the
    /// chain by appending a readback copy + map_async + sync fence in
    /// the SAME submit. Halves the driver-roundtrip latency vs the
    /// two-submit pattern (`submit_chain()` then `download_*`).
    ///
    /// On NVIDIA Vulkan each `queue.submit` costs 5-15 ms of driver
    /// overhead beyond the actual GPU work. Merging the submits is a
    /// pure win — the chain dispatch was always going to fence on the
    /// readback's `device.poll(Wait)` anyway.
    pub fn flush_chain_and_download_linear(&mut self, slot_a: bool, pixel_count: usize) -> Vec<f32> {
        if self.is_device_lost() {
            return vec![0.0f32; pixel_count * 4];
        }
        let byte_size = (pixel_count as u64) * 16;
        let staging = self.staging_read.as_ref().expect("staging buffer must exist");
        let source_buf = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let source_buf = source_buf.expect("chain buffers must be initialized");

        // Pull the chain encoder if one is open; otherwise create a
        // fresh encoder for the readback copy.
        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.encoder(Some("flush_chain_dl"))
        });
        let mut readback = self.readbacks.begin(byte_size);
        encoder.copy_buffer_to_buffer(source_buf, 0, staging, 0, byte_size);
        self.submit(encoder);

        let t_submit = std::time::Instant::now();
        let slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        self.flush();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });

        mark_gpu_stage(GpuStage::FlushChain);
        let result = match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let t_mapped = t_submit.elapsed();
                let data = slice.get_mapped_range();
                let result = mapped_bytes_to_f32(&data, pixel_count * 4);
                tracing::debug!(
                    "chain flush {} MB: gpu+transfer {:.1}ms, copy out {:.1}ms",
                    byte_size / 1_000_000,
                    t_mapped.as_secs_f64() * 1000.0,
                    (t_submit.elapsed() - t_mapped).as_secs_f64() * 1000.0,
                );
                drop(data);
                staging.unmap();
                readback.succeeded = true;
                result
            }
            other => {
                self.mark_device_lost(&format!("flush_chain map_async failed: {other:?}"));
                vec![0.0f32; pixel_count * 4]
            }
        };
        mark_gpu_stage(GpuStage::Idle);
        result
    }

    /// Borrow the chain buffers by their ping-pong slot. `slot_a` true
    /// returns `buf_a`; false returns `buf_b`. Intended for callers that
    /// want to pass the buffer refs to `*_chain` dispatch entries.
    #[allow(dead_code)]
    pub fn chain_buffer(&self, slot_a: bool) -> &wgpu::Buffer {
        if slot_a {
            self.buf_a.as_ref().expect("ensure_chain_buffers must be called first")
        } else {
            self.buf_b.as_ref().expect("ensure_chain_buffers must be called first")
        }
    }

    /// Apply a chain of effects on the GPU and read the picture back: the
    /// effects as the plan route runs them ([`Self::chain_run_effects`]:
    /// one encoder, a pass of their own only where one isn't recorded),
    /// with an upload before and a download after. Kept for the CPU/GPU
    /// parity tests and examples, which compare the GPU's effects with the
    /// CPU kernels' on plain frames; the renderer runs effects as plan
    /// nodes (phase 2b M2, step 11d). An effect left out (a lost device)
    /// gives the CPU kernels' picture instead.
    #[doc(hidden)]
    pub fn apply_effects(
        &mut self,
        effects: &[BuiltinEffect],
        linear_rgba: &[f32],
        width: u32,
        height: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() || effects.is_empty() {
            return linear_rgba.to_vec();
        }
        self.ensure_recording_buffers(width, height);
        self.upload(self.buf_a.as_ref().unwrap(), 0, bytemuck::cast_slice(linear_rgba));
        self.begin_chain();
        let run = self.chain_run_effects(effects, true, width, height);
        self.submit_chain();
        if !run.complete {
            return crate::apply_effects(effects, linear_rgba.to_vec(), width, height);
        }
        let final_buf = if run.in_a { self.buf_a.as_ref().unwrap() } else { self.buf_b.as_ref().unwrap() };
        self.download_linear_rgba_f32(final_buf, (width * height) as usize)
    }

    /// One break of a chain ([`Self::chain_run_effects`]): an effect, or a
    /// run of spills, the chain couldn't record, run in passes of its own
    /// on the chain buffers, on the frame in slot A (`in_a`) or B. Returns
    /// whether the result sits in slot A. Equalize comes here only when
    /// recording is refused (#1215), and builds its histogram on the CPU;
    /// a run of spills too long for the slots runs unrecorded. Any other
    /// effect is only ever recorded (the chain makes the layer passes'
    /// buffers before it records), so one that couldn't be is missing from
    /// the picture, and [`Self::last_effects_complete`] says so.
    pub(super) fn run_break_pass(&mut self, part: &[BuiltinEffect], in_a: bool, width: u32, height: u32) -> bool {
        // A lost device runs none of them, so every effect is missing;
        // set before the early return, or a caller would read the last
        // run's answer.
        self.effects_incomplete.set(self.is_device_lost() && !part.is_empty());
        if self.is_device_lost() || part.is_empty() {
            return in_a;
        }
        self.ensure_recording_buffers(width, height);
        let byte_size = u64::from(width) * u64::from(height) * 16;
        let (buf_a, buf_b) = (self.buf_a.as_ref().unwrap(), self.buf_b.as_ref().unwrap());
        let (input, output) = if in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
        let current_in_a = in_a;
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        if crate::layer::Spill::of(&part[0]).is_some() {
            let spills: Vec<_> = part.iter().filter_map(crate::layer::Spill::of).collect();
            return if self.layer_spills(input, output, width, height, &spills) { !in_a } else { in_a };
        }
        match &part[0] {
                BuiltinEffect::Equalize { strength } => {
                    // Hybrid GPU: build histogram/CDF on CPU (fast), remap on GPU (parallel).
                    // Step 1: Download current frame to compute histogram.
                    // The CDF of display-encoded luma, built as the CPU
                    // path builds it.
                    let current_buf = if current_in_a { buf_a } else { buf_b };
                    let frame_bytes = self.download(current_buf, byte_size);
                    let frame_data: &[f32] = bytemuck::cast_slice(&frame_bytes);

                    let cdf = crate::equalize_cdf(frame_data, self.luma_coefs);

                    // Step 4: Upload CDF as storage buffer and dispatch GPU remap shader
                    let cdf_bytes = bytemuck::cast_slice(&cdf);
                    debug_assert_eq!(cdf.len(), EQUALIZE_CDF_LEN);
                    self.upload(&self.equalize_cdf_buf, 0, cdf_bytes);

                    self.write_params(&EqualizeParams {
                        width,
                        height,
                        strength: *strength,
                        _pad0: 0,
                        luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
                    });

                    let input = if current_in_a { buf_a } else { buf_b };
                    let output = if current_in_a { buf_b } else { buf_a };

                    // Create bind group with 4 bindings (like huesat)
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("equalize_bg"),
                        layout: &self.pipelines.huesat_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: self.equalize_cdf_buf.as_entire_binding() },
                        ],
                    });

                    let mut encoder = self.encoder(None);
                    {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.pipelines.equalize_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        dispatch_flat(&mut pass, workgroups);
                    }
                    self.submit(encoder);
                    // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                    // queue drains between submits. See submit_chain for context.
                    self.device.poll(wgpu::Maintain::Poll);

                    !current_in_a
                }
                _ => {
                    self.effects_incomplete.set(true);
                    in_a
                }
            }
    }

    /// Make the buffers recorded effects use at `width` × `height`: the
    /// chain's frames (`buf_c` is a run's scratch) and the layer passes'
    /// own. A caller that records effects itself calls this once per
    /// output size, so whether a layer pass records never depends on
    /// whether a chain happened to run at that size before.
    #[doc(hidden)]
    pub fn ensure_recording_buffers(&mut self, width: u32, height: u32) {
        let byte_size = u64::from(width) * u64::from(height) * 16;
        self.ensure_buffers(byte_size);
        self.layer_gpu.ensure_buffers(&self.device, byte_size);
    }

    /// Run `effects` as nodes of the open chain (`begin_chain`), on the
    /// picture in slot A (`in_is_a`) or B (#1089, phase 2b M2, step 10).
    /// A recorded effect goes into the chain's own encoder, so a grade
    /// and the effects after it are one submit. One that isn't recorded
    /// is a break: the chain so far is submitted, the effect runs in its
    /// own pass on the same buffers, and a new chain segment opens; the
    /// picture never leaves the GPU. `buf_c` is the multi-pass scratch,
    /// so effects run after any secondary that keeps a picture there.
    #[doc(hidden)]
    pub fn chain_run_effects(&mut self, effects: &[BuiltinEffect], in_is_a: bool, width: u32, height: u32) -> ChainEffects {
        let mut out = ChainEffects { in_a: in_is_a, breaks: 0, complete: true };
        // This run's answer for `last_effects_complete`: a break that
        // leaves an effect out says so below.
        self.effects_incomplete.set(false);
        if self.is_device_lost() {
            out.complete = effects.is_empty();
            self.effects_incomplete.set(!out.complete);
            return out;
        }
        // Recording reads the layer passes' buffers too: made now, so
        // whether an effect records never depends on an earlier run.
        self.ensure_recording_buffers(width, height);
        let spills = |e: &BuiltinEffect| crate::layer::Spill::of(e).is_some();
        let mut rest = effects;
        while !rest.is_empty() {
            // Effects that spill past the picture are one layer: each is
            // cast from the same picture and stacked under it, so a run of
            // them goes as one, as `apply_effects` groups them.
            let n = if spills(&rest[0]) { rest.iter().take_while(|e| spills(e)).count() } else { 1 };
            let (part, after) = rest.split_at(n);
            rest = after;
            let mut outcome = self.record_in_chain(part, out.in_a, width, height);
            if outcome == Recorded::OutOfSlots {
                // The chain's encoder holds the slots' readers: submit it,
                // then they are free, and the chain goes on in a new one.
                self.submit_chain();
                self.begin_chain();
                outcome = self.record_in_chain(part, out.in_a, width, height);
            }
            match outcome {
                Recorded::Done => out.in_a = !out.in_a,
                Recorded::Identity => {}
                Recorded::NotRecordable | Recorded::OutOfSlots => {
                    for fx in part {
                        crate::residency::record(crate::residency::effect_id(fx), "GPU recorder declined; chain break", crate::residency::Work::Fallback);
                    }
                    out.breaks += 1;
                    self.submit_chain();
                    out.in_a = self.run_break_pass(part, out.in_a, width, height);
                    out.complete &= self.last_effects_complete();
                    self.begin_chain();
                }
            }
        }
        // A later clean break must not hide an earlier one that left an
        // effect out: the run's answer is all of them.
        self.effects_incomplete.set(!out.complete);
        out
    }

    /// A window's shapes as the buffers the window mix reads (#1252): its
    /// shape records and its bezier points. Each call gets its own buffers:
    /// several masked effects in one encoder would otherwise share one
    /// upload, and the queue writes all land before the encoder runs.
    pub(super) fn window_buffers(&self, masks: &[GpuMaskDef]) -> (wgpu::Buffer, wgpu::Buffer) {
        let (records, points) = Self::build_mask_storage(masks);
        let records: &[u8] = if records.is_empty() {
            &[0u8; std::mem::size_of::<MaskRecord>()]
        } else {
            bytemuck::cast_slice(&records)
        };
        let points: &[u8] = if points.is_empty() { &[0u8; 8] } else { bytemuck::cast_slice(&points) };
        let make = |label: &str, bytes: &[u8]| {
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buf, 0, bytes);
            buf
        };
        (make("window_records", records), make("window_points", points))
    }

    /// [`Self::chain_run_effects`], mixed back over the picture it started
    /// from (phase 2b M2, step 9c): by `mix`, and by the coverage `mask`
    /// gives (straight RGBA of the frame's size, in a pool slot; a window's
    /// shapes, worked out inside the mix with no slot, #1252; none is
    /// white). The input is kept in a pool slot before the effects
    /// run, and the mix writes the other chain buffer, so nothing aliases.
    /// `None` when the pool's budget refuses, before anything is recorded:
    /// the caller runs the node another way.
    #[doc(hidden)]
    pub fn chain_run_effects_mixed(
        &mut self,
        effects: &[BuiltinEffect],
        in_is_a: bool,
        width: u32,
        height: u32,
        mix: crate::mask_mix::MaskMix,
        mask: Option<EffectMatte<'_>>,
    ) -> Option<ChainEffects> {
        // One effect recorded as a single dispatch writes the other chain
        // buffer and leaves its input where it is: the mix reads it there
        // and writes over it (#1254). Several effects, or one of several
        // passes, may write over the input, and a copy of it is kept.
        let in_place = matches!(effects, [one] if Self::single_dispatch(one));
        self.chain_run_mixed(in_place, in_is_a, width, height, mix, mask, |gpu| gpu.chain_run_effects(effects, in_is_a, width, height))
    }

    /// A merge on the chain (`apply_merge_chain`) mixed back over the
    /// picture it lays over by `mix` and the coverage `mask` gives
    /// (#2758): the merge's picture laid only where the mask is, as a
    /// masked effect's result is. The merge is one dispatch into the other
    /// chain buffer, its input left where it is.
    #[allow(clippy::too_many_arguments)]
    pub fn chain_merge_mixed(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        opacity: f32,
        mode: u32,
        other: &KeptPicture,
        mix: crate::mask_mix::MaskMix,
        mask: Option<EffectMatte<'_>>,
    ) -> Option<ChainEffects> {
        self.chain_run_mixed(true, in_is_a, width, height, mix, mask, |gpu| {
            gpu.apply_merge_chain(in_is_a, width, height, opacity, mode, other);
            ChainEffects { in_a: !in_is_a, breaks: 0, complete: !gpu.is_device_lost() }
        })
    }

    /// `body` run on the chain, mixed back over the picture it started
    /// from by `mix` and `mask`, as [`Self::chain_run_effects_mixed`]
    /// says; `in_place` when `body` is one dispatch that leaves its input
    /// where it is, so no copy of it is kept.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn chain_run_mixed(
        &mut self,
        in_place: bool,
        in_is_a: bool,
        width: u32,
        height: u32,
        mix: crate::mask_mix::MaskMix,
        mask: Option<EffectMatte<'_>>,
        body: impl FnOnce(&mut Self) -> ChainEffects,
    ) -> Option<ChainEffects> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        let bytes = u64::from(width) * u64::from(height) * 16;
        let image = SlotKey { width, height, usage: SlotUsage::Image };
        let make = |device: &wgpu::Device, k: SlotKey| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(match k.usage { SlotUsage::Image => "slot_image", SlotUsage::Matte => "slot_matte" }),
                size: u64::from(k.width) * u64::from(k.height) * 16,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let device = &self.device;
        let kept = if in_place { None } else { Some(self.slot_pool.lease(image, |k| make(device, k)).ok()?) };
        let keyed = matches!(mask, Some(EffectMatte::Keyed(_)));
        // A window is drawn inside the mix itself: its shapes, no matte.
        let window = match mask {
            Some(EffectMatte::Shapes(masks)) => Some((self.window_buffers(masks), masks.len() as u32)),
            _ => None,
        };
        let matte = match mask {
            Some(EffectMatte::Keyed(m)) => Some(m.0),
            Some(EffectMatte::Shapes(_)) | None => None,
            Some(source) => {
                let device = &self.device;
                // Uploaded, so it lands before the open encoder: a slot no
                // pass of it has used (a keyed matte read earlier in it).
                match self.slot_pool.lease_for_upload(SlotKey { usage: SlotUsage::Matte, ..image }, |k| make(device, k)) {
                    Ok(id) => {
                        match source {
                            EffectMatte::Pixels(data) => {
                                self.queue.write_buffer(self.slot_pool.buffer(id), 0, bytemuck::cast_slice(data));
                                self.slot_pool.mark_cpu_written(id);
                            }
                            EffectMatte::Shapes(_) | EffectMatte::Keyed(_) => unreachable!("taken above"),
                        }
                        Some(id)
                    }
                    Err(_) => {
                        if let Some(kept) = kept {
                            self.slot_pool.release(kept);
                        }
                        return None;
                    }
                }
            }
        };
        // The input, kept before the effects can write over it.
        if let Some(kept) = kept {
            let src = if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            enc.copy_buffer_to_buffer(src.expect("chain buffers"), 0, self.slot_pool.buffer(kept), 0, bytes);
        }
        let mut run = body(self);
        // A break runs the effect in passes of its own, which this didn't
        // keep its input from: the run is said incomplete, and the caller
        // takes another route, as for a mix that wasn't recorded.
        if in_place && run.breaks > 0 {
            run.complete = false;
            if let Some(m) = matte.filter(|_| !keyed) {
                self.slot_pool.release(m);
            }
            return Some(run);
        }
        // The mix's uniforms are claimed once the effects are recorded: an
        // effect that breaks the chain resets the ring. A full ring is
        // submitted and the chain goes on in a new one.
        let claim = |gpu: &mut Self| match &window {
            Some((_, count)) => gpu.claim_mask_mix_window(width, height, mix, *count),
            None => gpu.claim_mask_mix(width, height, mix, matte.is_some(), false),
        };
        let mut params = claim(self);
        if params == Err(crate::mask_mix::MaskMixError::ParamsExhausted) {
            self.submit_chain();
            self.begin_chain();
            params = claim(self);
        }
        let recorded = match params {
            Ok(Some(slot)) => {
                let mut encoder = self.chain_encoder.take().expect(Self::CHAIN_ENCODER_EXPECT);
                let (a, b) = (self.buf_a.as_ref().expect("chain buffers"), self.buf_b.as_ref().expect("chain buffers"));
                let (effect, out) = if run.in_a { (a, b) } else { (b, a) };
                // The picture under the effect: its kept copy; in place, the
                // buffer the effect read, which the mix writes (`out`); or,
                // when the effect wrote nothing, the effect's buffer itself.
                let input = match kept {
                    Some(kept) => Some(self.slot_pool.buffer(kept)),
                    None if run.in_a == in_is_a => Some(effect),
                    None => None,
                };
                let r = match &window {
                    Some(((records, points), _)) => self.record_mask_mix_window_in(&mut encoder, input, effect, records, points, out, width, height, slot),
                    None => self.record_mask_mix_in(&mut encoder, input, effect, matte.map(|m| self.slot_pool.buffer(m)), out, width, height, slot),
                };
                self.chain_encoder = Some(encoder);
                r
            }
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        // Their readers are recorded: returned (a CPU-written matte is held
        // until the encoder is submitted).
        if let Some(kept) = kept {
            self.slot_pool.release(kept);
        }
        // A keyed matte is its key's, for its other readers.
        if let Some(m) = matte.filter(|_| !keyed) {
            self.slot_pool.release(m);
        }
        match recorded {
            Ok(()) => run.in_a = !run.in_a,
            // The effects ran, the mix didn't: the picture lacks the mix,
            // and the run says so (the caller takes another route).
            Err(e) => {
                tracing::warn!("an effect's mix wasn't recorded: {e}");
                run.complete = false;
            }
        }
        Some(run)
    }

    /// Whether `effect` is recorded as one dispatch from its input into
    /// the other chain buffer, leaving its input as it was (#1254): the
    /// one-slot effects, but for those that run passes of their own
    /// (a denoise's iterations, a card's rounded corners).
    pub(super) fn single_dispatch(effect: &BuiltinEffect) -> bool {
        slots_needed(effect) == Some(1) && !matches!(effect, BuiltinEffect::Denoise { .. } | BuiltinEffect::RoundedCorners { .. })
    }

    /// The run is over: every pool slot is free again, and a size no run
    /// has used for a while is dropped. Gives back the most slots leased
    /// at once.
    #[doc(hidden)]
    pub fn end_slot_run(&mut self) -> usize {
        let peak = self.slot_pool.end_run();
        self.trim_if_pressed();
        self.report_memory(peak);
        peak
    }

    /// The GPU ran out of memory since the last run (#3174): what its
    /// caches keep for reuse is let go, the slots, the stack's layers, the
    /// titles' sprites, the flow pairs and the neighbours kept, and the
    /// device waited on so it is freed before the next frame asks for
    /// more. The chain's own buffers stay, the next frame needs them.
    fn trim_if_pressed(&mut self) {
        let now = crate::gpu_context::memory_pressure();
        if now == self.pressure_seen {
            return;
        }
        self.pressure_seen = now;
        self.slot_pool.retain_keys(&[]);
        self.stack_layers.clear();
        self.stack_acc = None;
        self.stack_buf_size = 0;
        self.stack_sprites.clear();
        if let Some(flow) = self.flow_gpu.as_mut() {
            flow.let_go();
        }
        self.neighbour_pool.lock().unwrap_or_else(|e| e.into_inner()).trim_if_pressed();
        let _ = self.device.poll(wgpu::Maintain::Wait);
        tracing::warn!("GPU memory ran out: the effect caches let go of what they keep, and the next frame tries again");
    }

    /// What each of its caches holds now, into the GPU memory report
    /// (#3174): read at a run's end, after the slots unused for a while
    /// are dropped; a slot "in use" is the most leased at once in the run.
    fn report_memory(&mut self, peak_slots: usize) {
        use crate::gpu_memory::Holding;
        let kept: Vec<u64> = [&self.buf_a, &self.buf_b, &self.buf_c, &self.matte_buf, &self.staging_read].iter().filter_map(|b| b.as_ref().map(wgpu::Buffer::size)).collect();
        self.memory_gauges.chain.report_holding(kept.iter().sum(), Holding { count: kept.len() as u64, in_use: 0, each: kept.first().copied().unwrap_or(0), bytes_per_pixel: 16 });
        let slots = self.slot_pool.bytes(|k| u64::from(k.width) * u64::from(k.height) * 16);
        let count = self.slot_pool.len() as u64;
        self.memory_gauges.slots.report_holding(slots, Holding { count, in_use: peak_slots as u64, each: slots.checked_div(count).unwrap_or(0), bytes_per_pixel: 16 });
        let layers = self.stack_layers.len() as u64 + u64::from(self.stack_acc.is_some());
        self.memory_gauges.stack.report_holding(layers * self.stack_buf_size, Holding { count: layers, in_use: 0, each: self.stack_buf_size, bytes_per_pixel: 16 });
        let sprites: u64 = self.stack_sprites.iter().map(|s| s.buffer.size() + s.units.as_ref().map_or(0, |u| u.4.size())).sum();
        let n = self.stack_sprites.len() as u64;
        self.memory_gauges.sprites.report_holding(sprites, Holding { count: n, in_use: 0, each: sprites.checked_div(n).unwrap_or(0), bytes_per_pixel: 0 });
        if let Some(flow) = self.flow_gpu.as_mut() {
            flow.report_memory();
        }
    }

    /// Record one effect, or one run of spills, into the open chain
    /// encoder.
    pub(super) fn record_in_chain(&mut self, part: &[BuiltinEffect], in_is_a: bool, width: u32, height: u32) -> Recorded {
        let (Some(mut encoder), Some(mut slots)) = (self.chain_encoder.take(), self.effect_params.take()) else {
            return Recorded::NotRecordable;
        };
        let (a, b) = (self.buf_a.as_ref().expect("chain buffers"), self.buf_b.as_ref().expect("chain buffers"));
        let (src, dst) = if in_is_a { (a, b) } else { (b, a) };
        let rec = &mut EffectRecording { encoder: &mut encoder, slots: &mut slots };
        let outcome = match part {
            [one] if crate::layer::Spill::of(one).is_none() => self.record_effect(one, src, dst, self.buf_c.as_ref(), width, height, rec),
            spills => self.record_spill_run(spills, src, dst, self.buf_c.as_ref(), width, height, rec),
        };
        self.chain_encoder = Some(encoder);
        self.effect_params = Some(slots);
        outcome
    }

    /// Whether the last chain's effects ([`Self::chain_run_effects`],
    /// its breaks included) were all applied. `false` when one that is only ever recorded could not be
    /// (a lost device): its picture is missing that effect, and the caller
    /// takes another path or declines the frame, never shows it.
    pub fn last_effects_complete(&self) -> bool {
        !self.effects_incomplete.get()
    }

    /// For tests: make every `record_effect` answer `NotRecordable`, as a
    /// lost device does, to prove a run reports what it left out.
    #[doc(hidden)]
    pub fn refuse_recording(&mut self, refuse: bool) {
        self.refuse_recording = refuse;
    }
}


