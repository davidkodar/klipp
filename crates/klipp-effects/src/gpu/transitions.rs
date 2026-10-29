//! Transitions on the GPU (#2890): a transition between two frames, the
//! Pieces and Shutter transitions, and the colour fade, with their params and
//! shaders.

use super::*;

impl GpuEffectsPipeline {
    /// A transition between layers `from` and `to` into layer `out`. The
    /// sides are mixed premultiplied and the result divided back to
    /// straight, as the CPU mixes them: a side that is partly transparent
    /// (scaled down, matted, faded) must lend its colour by its alpha, not
    /// by the blend's weight alone (#718, #1026). With both sides opaque
    /// it is the plain mix.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_transition(&mut self, from: usize, to: usize, out: usize, kind_id: u32, progress: f32, p: [f32; 4], color: [f32; 4]) -> bool {
        if crate::pieces::is_posed(kind_id, p[3]) {
            let (width, height) = self.stack_dims;
            return self.stack_pieces(from, to, out, &crate::pieces::plan(kind_id, progress, width, height, p, 0.0, 1));
        }
        if self.is_device_lost() || from >= self.stack_layers.len() || to >= self.stack_layers.len() || out >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let (Some(from_pm), Some(to_pm)) = (self.buf_a.as_ref(), self.buf_b.as_ref()) else { return false };
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 0, _pad: 0 });
        let (from_layer, to_layer) = (std::sync::Arc::clone(&self.stack_layers[from]), std::sync::Arc::clone(&self.stack_layers[to]));
        self.stack_pass(&self.pipelines.alpha_form_pipeline, &from_layer, from_pm, wg, "stack_transition_premultiply_from");
        self.stack_pass(&self.pipelines.alpha_form_pipeline, &to_layer, to_pm, wg, "stack_transition_premultiply_to");
        self.write_params(&TransitionParams {
            width, height, progress, kind_id,
            param0: p[0], param1: p[1], param2: p[2], param3: p[3],
            color_r: color[0], color_g: color[1], color_b: color[2], color_a: color[3],
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stack_transition_bg"),
            layout: &self.pipelines.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: from_pm.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: to_pm.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.buf_c.as_ref().unwrap().as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(Some("stack_transition"));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.transition_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, wg);
        }
        self.submit(enc);
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 1, _pad: 0 });
        let out_layer = std::sync::Arc::clone(&self.stack_layers[out]);
        self.stack_pass(&self.pipelines.alpha_form_pipeline, self.buf_c.as_ref().unwrap(), &out_layer, wg, "stack_transition_unpremultiply");
        true
    }

    /// Render a transition between two frames on the GPU.
    /// `kind_id`: 0=CrossDissolve, 1=WipeLeft, 2=WipeRight, 3=WipeUp, 4=WipeDown, 5=DipToBlack, 6=DipToWhite
    pub fn render_transition(
        &mut self,
        from_rgba: &[u8],
        to_rgba: &[u8],
        kind_id: u32,
        progress: f32,
        width: u32,
        height: u32,
        user_param0: f32,
        user_param1: f32,
        user_param2: f32,
        user_param3: f32,
        user_color: [f32; 4],
    ) -> Vec<u8> {
        if self.is_device_lost() {
            let _ = to_rgba;
            return from_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        // Upload: buf_a = from, buf_c = to, buf_b = output.
        // u8 sRGB → linear f32 at the boundary.
        self.upload_u8_rgba_as_linear(buf_a, from_rgba);
        self.upload_u8_rgba_as_linear(buf_c, to_rgba);

        self.write_params(&TransitionParams {
            width,
            height,
            progress,
            kind_id,
            param0: user_param0,
            param1: user_param1,
            param2: user_param2,
            param3: user_param3,
            color_r: user_color[0],
            color_g: user_color[1],
            color_b: user_color[2],
            color_a: user_color[3],
        });

        // Create transition bind group (from, to, output, params)
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipelines.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_c.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.transition_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_as_u8(buf_b, (width * height) as usize)
    }

    /// Linear-f32 input/output transition. Identical shader work to
    /// [`Self::render_transition`] but skips the sRGB encode on the output
    /// path *and* the linear decode on the input path — callers
    /// already speaking linear (the engine compositor + display
    /// chain) hand off and pick up linear-f32 buffers directly,
    /// shaving the two CPU pow loops that the u8 boundary version
    /// pays per frame.
    ///
    /// The compute shader itself doesn't care about the colour
    /// space — it just lerps RGBA. By keeping the boundary in
    /// linear f32 we avoid round-tripping a colour-space encode
    /// purely to satisfy a u8 storage format.
    #[allow(clippy::too_many_arguments)]
    pub fn render_transition_linear(
        &mut self,
        from_linear: &[f32],
        to_linear: &[f32],
        kind_id: u32,
        progress: f32,
        width: u32,
        height: u32,
        user_param0: f32,
        user_param1: f32,
        user_param2: f32,
        user_param3: f32,
        user_color: [f32; 4],
    ) -> Vec<f32> {
        if crate::pieces::is_posed(kind_id, user_param3) {
            let plan = crate::pieces::plan(kind_id, progress, width, height, [user_param0, user_param1, user_param2, user_param3], 0.0, 1);
            return self.render_pieces_linear(from_linear, to_linear, &plan);
        }
        if self.is_device_lost() {
            let _ = to_linear;
            return from_linear.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        // Upload linear-f32 directly — no sRGB conversion needed.
        self.upload(buf_a, 0, bytemuck::cast_slice(from_linear));
        self.upload(buf_c, 0, bytemuck::cast_slice(to_linear));

        self.write_params(&TransitionParams {
            width,
            height,
            progress,
            kind_id,
            param0: user_param0,
            param1: user_param1,
            param2: user_param2,
            param3: user_param3,
            color_r: user_color[0],
            color_g: user_color[1],
            color_b: user_color[2],
            color_a: user_color[3],
        });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("render_transition_linear_bg"),
            layout: &self.pipelines.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_c.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut encoder = self.encoder(Some("render_transition_linear"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.transition_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// A posed-pieces plan in a buffer of its own (#985): a batched stack
    /// may not have read the last one yet.
    pub(super) fn pieces_buffer(&self, plan: &crate::pieces::PiecePlan) -> wgpu::Buffer {
        let mut data = plan.data.clone();
        if data.is_empty() {
            data.resize(crate::pieces::STRIDE, 0.0);
        }
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pieces"),
            size: (data.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        buf.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(&data));
        buf.unmap();
        buf
    }

    /// `SHADER_PIECES` over `from` and `to` into `out`, with `plan`.
    pub(super) fn dispatch_pieces(&self, from: &wgpu::Buffer, to: &wgpu::Buffer, out: &wgpu::Buffer, plan: &crate::pieces::PiecePlan, label: &str) {
        let full = (plan.width as usize) * (plan.height as usize);
        let matte = plan.matte.as_deref().filter(|m| plan.subject != 0 && m.len() >= full);
        let subject = if plan.subject == 0 { 0 } else if matte.is_some() { 2 } else { 1 };
        self.write_params(&PiecesParams { width: plan.width, height: plan.height, count: plan.count, samples: plan.samples, background_to: plan.background_to as u32, subject, _pad1: 0, _pad2: 0 });
        let pieces = self.pieces_buffer(plan);
        // A buffer of its own, as the plan's: one value when there's none.
        let matte_data: &[f32] = matte.map_or(&[0.0][..], |m| &m[..full]);
        let matte_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pieces_matte"),
            size: (matte_data.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        matte_buf.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(matte_data));
        matte_buf.unmap();
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.pipelines.pieces_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: from.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: to.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: pieces.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: matte_buf.as_entire_binding() },
            ],
        });
        let mut enc = self.encoder(Some(label));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.pieces_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, (plan.width * plan.height).div_ceil(WORKGROUP_SIZE));
        }
        self.submit(enc);
    }

    /// Posed piece transitions (#985) between two linear-light frames:
    /// what `pieces::render` draws on the CPU.
    pub fn render_pieces_linear(&mut self, from_linear: &[f32], to_linear: &[f32], plan: &crate::pieces::PiecePlan) -> Vec<f32> {
        if self.is_device_lost() {
            return crate::pieces::render(from_linear, to_linear, plan);
        }
        self.ensure_buffers((plan.width * plan.height * 16) as u64);
        let (buf_a, buf_b, buf_c) = (self.buf_a.as_ref().unwrap(), self.buf_b.as_ref().unwrap(), self.buf_c.as_ref().unwrap());
        self.upload(buf_a, 0, bytemuck::cast_slice(from_linear));
        self.upload(buf_c, 0, bytemuck::cast_slice(to_linear));
        self.dispatch_pieces(buf_a, buf_c, buf_b, plan, "render_pieces_linear");
        self.device.poll(wgpu::Maintain::Poll);
        self.download_linear_rgba_f32(buf_b, (plan.width * plan.height) as usize)
    }

    /// Layer `i` smeared across the camera's shutter (#1923): placed by
    /// each of `placements` (one Transform a shutter sample) and averaged
    /// premultiplied, given back straight, as the CPU's `shuttered`
    /// averages (#2017). One pass over the whole frame into `buf_c`, then
    /// back into the layer: a placed sample can reach past where the
    /// picture was. `false` for anything but Transforms, or no layer.
    pub fn stack_shutter(&mut self, i: usize, placements: &[BuiltinEffect]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() || placements.is_empty() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let rows: Option<Vec<[f32; 16]>> = placements.iter().map(|e| TransformParams::of(e, width, height).map(|p| p.shutter_row())).collect();
        let (Some(rows), Some(out)) = (rows, self.buf_c.as_ref()) else { return false };
        let layer = &self.stack_layers[i];
        let mut enc = self.shutter_pass(layer, &rows, out, rows.len() as u32, 0);
        enc.copy_buffer_to_buffer(out, 0, layer, 0, self.stack_pixel_count() * 16);
        self.submit(enc);
        true
    }

    /// Layer `i` as `placement` (a Transform) places it, added premultiplied
    /// to the shutter's running sum, which `first` starts afresh: one sample
    /// of a picture that is drawn anew at each instant the shutter is open,
    /// as the CPU's `motion_blurred` adds a title redrawn at each.
    /// [`Self::stack_shutter_finish`] gives the average back.
    pub fn stack_shutter_add(&mut self, i: usize, placement: &BuiltinEffect, first: bool) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let Some(row) = TransformParams::of(placement, width, height).map(|p| p.shutter_row()) else { return false };
        let size = self.stack_pixel_count() * 16;
        if self.shutter_sum.as_ref().is_none_or(|b| b.size() != size) {
            self.shutter_sum = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shutter_sum"),
                size,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }));
        }
        let Some(sum) = self.shutter_sum.as_ref() else { return false };
        let enc = self.shutter_pass(&self.stack_layers[i], &[row], sum, 1, if first { 1 } else { 3 });
        self.submit(enc);
        true
    }

    /// The shutter's running sum of `n` samples into layer `i`, averaged
    /// premultiplied and given back straight, as `stack_shutter` gives its
    /// own.
    pub fn stack_shutter_finish(&mut self, i: usize, n: u32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() || n == 0 {
            return false;
        }
        let Some(sum) = self.shutter_sum.as_ref() else { return false };
        // The pass reads no placement in this mode, but its binding wants one.
        let enc = self.shutter_pass(sum, &[[0.0; 16]], &self.stack_layers[i], n, 2);
        self.submit(enc);
        true
    }

    /// One shutter pass over the frame, `input` to `output`, in `mode`
    /// (the shader's), with `rows` as its samples: the encoder, unsubmitted.
    pub(super) fn shutter_pass(&self, input: &wgpu::Buffer, rows: &[[f32; 16]], output: &wgpu::Buffer, n: u32, mode: u32) -> wgpu::CommandEncoder {
        let (width, height) = self.stack_dims;
        let samples = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shutter_samples"),
            size: (rows.len() * 64) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        samples.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(rows));
        samples.unmap();
        self.write_params(&ShutterParams { width, height, n, mode });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stack_shutter"),
            layout: &self.pipelines.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: samples.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(Some("stack_shutter"));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.shutter_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE));
        }
        enc
    }

    /// Posed piece transitions (#985) on the stack: layers `from` and `to`
    /// into `out`, mixed premultiplied as `stack_transition` mixes.
    pub fn stack_pieces(&mut self, from: usize, to: usize, out: usize, plan: &crate::pieces::PiecePlan) -> bool {
        if self.is_device_lost() || from >= self.stack_layers.len() || to >= self.stack_layers.len() || out >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        if (plan.width, plan.height) != (width, height) {
            return false;
        }
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let (Some(from_pm), Some(to_pm)) = (self.buf_a.as_ref(), self.buf_b.as_ref()) else { return false };
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 0, _pad: 0 });
        let (from_layer, to_layer) = (std::sync::Arc::clone(&self.stack_layers[from]), std::sync::Arc::clone(&self.stack_layers[to]));
        self.stack_pass(&self.pipelines.alpha_form_pipeline, &from_layer, from_pm, wg, "stack_pieces_premultiply_from");
        self.stack_pass(&self.pipelines.alpha_form_pipeline, &to_layer, to_pm, wg, "stack_pieces_premultiply_to");
        self.dispatch_pieces(from_pm, to_pm, self.buf_c.as_ref().unwrap(), plan, "stack_pieces");
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 1, _pad: 0 });
        let out_layer = std::sync::Arc::clone(&self.stack_layers[out]);
        self.stack_pass(&self.pipelines.alpha_form_pipeline, self.buf_c.as_ref().unwrap(), &out_layer, wg, "stack_pieces_unpremultiply");
        true
    }

    /// One pass over two inputs (bindings 0 and 1) into `output`, with
    /// the params already written: the layout the blend shaders share.
    pub(super) fn dispatch_pair(&self, pipeline: &wgpu::ComputePipeline, a: &wgpu::Buffer, b: &wgpu::Buffer, output: &wgpu::Buffer, workgroups: u32) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pair_bg"),
            layout: &self.pipelines.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(None);
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(enc);
        self.device.poll(wgpu::Maintain::Poll);
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ColorFadeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) progress: f32,
    pub(super) _pad: u32,
    /// Linear RGB and alpha.
    pub(super) color: [f32; 4],
}

/// The shutter pass's size and sample count (#1923).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ShutterParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) n: u32,
    pub(super) mode: u32,
}

/// `SHADER_PIECES`' uniform (#985): the frame, and how many posed pieces
/// an instant and instants the plan holds.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct PiecesParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) count: u32,
    pub(super) samples: u32,
    pub(super) background_to: u32,
    /// 1 when the subject is cut out (#985 phase 3), 2 with its matte.
    pub(super) subject: u32,
    pub(super) _pad1: u32,
    pub(super) _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct TransitionParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) progress: f32,
    pub(super) kind_id: u32, // 0=CrossDissolve, 1=WipeLeft, 2=WipeRight, 3=WipeUp, 4=WipeDown, 5=DipToBlack, 6=DipToWhite
    // Generic per-kind user params. Meaning is a per-kind contract — see the
    // WGSL shader branches. Keep field layout identical to the WGSL struct.
    pub(super) param0: f32,
    pub(super) param1: f32,
    pub(super) param2: f32,
    pub(super) param3: f32,
    pub(super) color_r: f32,
    pub(super) color_g: f32,
    pub(super) color_b: f32,
    pub(super) color_a: f32,
}

/// A layer smeared across the camera's shutter (#1923), in one pass: each
/// pixel is the mean of the layer as each sample's transform places it,
/// the Transform's own inverse map and bilinear sample (transparent
/// outside the picture), averaged premultiplied and given back straight,
/// as the CPU's `shuttered` averages (#2017). `samples` holds a
/// `Sample` of four vec4s each: anchor and position; rotation and inverse
/// scale; tilt; focal length.
pub(super) const SHADER_SHUTTER: &str = concat!(
    // Two parts around the alpha floor, which the shader uses inside an
    // expression: an include can only stand for whole lines (#2890).
    gpu_shader!("transitions/shutter.1.wgsl"),
    unpremultiply_min_alpha_wgsl!(),
    gpu_shader!("transitions/shutter.2.wgsl"),
);

// Colour fade — from a solid colour into the picture (or out to it):
// the pixel mixed with the colour by `progress`, in linear light, as a
// dissolve mixes.
pub(super) const SHADER_COLOR_FADE: &str = gpu_shader!("transitions/color_fade.wgsl");

pub(super) const SHADER_TRANSITION: &str = gpu_shader!("transitions/transition.wgsl");

/// Posed piece transitions (#985, phase 2): the plan `pieces::plan` works
/// out (each piece's screen-to-picture homography, box, edges, light,
/// opacity and shadow, nearest first, an instant at a time) drawn front
/// to back over what shows behind, shadows on that, instants averaged.
/// `pieces::render` is its CPU twin, line for line.
pub(super) const SHADER_PIECES: &str = gpu_shader!("transitions/pieces.wgsl");
