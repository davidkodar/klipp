//! Compositing on the GPU (#2890): a layer blended or merged over what is
//! under it, a picture mixed with one kept, and opacity, with their params and
//! shaders.

use super::*;

impl GpuEffectsPipeline {
    /// Keep a copy of the chain's current picture (in the slot `in_is_a`
    /// names) for [`Self::apply_mix_with_kept_chain`]: a node that mixes
    /// its result with its input over several dispatches.
    pub fn keep_chain_input(&mut self, in_is_a: bool, width: u32, height: u32) {
        if self.is_device_lost() {
            return;
        }
        let size = (width * height * 16) as u64;
        if self.grade_mix_src.as_ref().is_none_or(|b| b.size() < size) {
            self.grade_mix_src = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("grade_mix_src"),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let src = if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }.expect(Self::CHAIN_BG_EXPECT);
        let dst = self.grade_mix_src.as_ref().unwrap();
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        enc.copy_buffer_to_buffer(src, 0, dst, 0, size);
    }

    /// Dispatch-only mix of the chain's picture with the one
    /// [`Self::keep_chain_input`] kept: `kept + (current - kept) * mix`.
    pub fn apply_mix_with_kept_chain(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuGradeMixParams { width, height, mix, _pad: 0 });
        let (a, b) = (self.buf_a.as_ref().expect(Self::CHAIN_BG_EXPECT), self.buf_b.as_ref().expect(Self::CHAIN_BG_EXPECT));
        let (input, output) = if in_is_a { (a, b) } else { (b, a) };
        let kept = self.grade_mix_src.as_ref().expect("keep_chain_input first");
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_grade_mix"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: kept.as_entire_binding() },
            ],
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.grade_mix_pipeline, &bind_group, workgroups);
    }

    /// Dispatch-only Blend: `a + (b - a) * mix`, `a` the chain's
    /// picture and `b` the kept picture `other` (M3 step 17b). Alpha
    /// mixes with the rest.
    pub fn apply_blend_chain(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32, other: &KeptPicture) {
        self.dispatch_composite(in_is_a, width, height, mix, /* over = */ false, other);
    }

    /// Dispatch-only Layer: the over operator, foreground from the
    /// chain and background the kept picture `other`.
    pub fn apply_over_chain(&mut self, in_is_a: bool, width: u32, height: u32, other: &KeptPicture) {
        self.dispatch_composite(in_is_a, width, height, 1.0, /* over = */ true, other);
    }

    /// Dispatch-only merge (#2758): the kept picture `other` laid over
    /// the chain's as a layer mixer's top layer over its base, by `mode`
    /// (`BlendMode as u32`) and `opacity` times its own alpha, the
    /// chain's alpha kept: `layer_mixer_compose` for two layers.
    pub fn apply_merge_chain(&mut self, in_is_a: bool, width: u32, height: u32, opacity: f32, mode: u32, other: &KeptPicture) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuMergeParams { width, height, opacity, mode });
        let (a, b) = (self.buf_a.as_ref().expect(Self::CHAIN_BG_EXPECT), self.buf_b.as_ref().expect(Self::CHAIN_BG_EXPECT));
        let (input, output) = if in_is_a { (a, b) } else { (b, a) };
        let other = self.slot_pool.buffer(other.0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_merge_over"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: other.as_entire_binding() },
            ],
        });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.merge_over_pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    pub(super) fn dispatch_composite(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32, over: bool, other: &KeptPicture) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuGradeMixParams { width, height, mix, _pad: 0 });
        let (a, b) = (
            self.buf_a.as_ref().expect(Self::CHAIN_BG_EXPECT),
            self.buf_b.as_ref().expect(Self::CHAIN_BG_EXPECT),
        );
        let (input, output) = if in_is_a { (a, b) } else { (b, a) };
        let other = self.slot_pool.buffer(other.0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_composite"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: other.as_entire_binding() },
            ],
        });
        let pipeline = if over { &self.pipelines.layer_over_pipeline } else { &self.pipelines.blend_mix_pipeline };
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, pipeline, &bind_group, workgroups);
    }

    /// A slowed clip's in-between frame on the chain (#2854): the chain's
    /// picture, or `from` in its place (another source frame on the GPU),
    /// cross-faded `mix` of the way toward `toward` (linear RGBA, `width`
    /// × `height`, a source frame on the GPU), into the chain's other
    /// buffer, one dispatch. `false` when it couldn't be recorded.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_mix_toward_chain(&mut self, in_is_a: bool, width: u32, height: u32, from: Option<&wgpu::Buffer>, mix: f32, toward: &wgpu::Buffer) -> bool {
        let bytes = u64::from(width) * u64::from(height) * 16;
        if self.is_device_lost() || self.chain_encoder.is_none() || toward.size() < bytes || from.is_some_and(|f| f.size() < bytes) {
            return false;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuGradeMixParams { width, height, mix, _pad: 0 });
        let (input, output) = match (self.buf_a.as_ref(), self.buf_b.as_ref()) {
            (Some(a), Some(b)) if in_is_a => (from.unwrap_or(a), b),
            (Some(a), Some(b)) => (from.unwrap_or(b), a),
            _ => return false,
        };
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_mix_toward"),
            layout: &self.pipelines.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: toward.as_entire_binding() },
            ],
        });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.blend_mix_pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
        true
    }

    /// Blend layer `i` onto the accumulator with `opacity` and `mode`
    /// (`BlendMode as u32`, the order of `klipp_effects::BlendMode`; 13
    /// is a plain lerp). The first Normal layer at full opacity is
    /// copied in, as the CPU stack moves it in.
    pub fn stack_blend(&mut self, i: usize, opacity: f32, mode: u32) -> bool {
        let h = self.stack_dims.1;
        self.stack_blend_rows(i, opacity, mode, 0, h)
    }

    /// `stack_blend` over rows `y0..y1` only.
    pub fn stack_blend_rows(&mut self, i: usize, opacity: f32, mode: u32, y0: u32, y1: u32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let (y0, y1) = (y0.min(height), y1.min(height));
        if !self.stack_acc_touched && mode == 0 && opacity >= 1.0 && y0 == 0 && y1 == height {
            let acc = self.stack_acc.clone().unwrap();
            if !self.stack_copy(&self.stack_layers[i], &acc, "stack_move_in") {
                return false;
            }
            self.stack_acc_touched = true;
            return true;
        }
        if !self.stack_ensure_acc() || y1 <= y0 {
            return self.stack_acc.is_some();
        }
        self.write_params(&BlendLayerParams { width, height, opacity, mode, y0, y1, _p0: 0, _p1: 0 });
        let acc = self.stack_acc.clone().unwrap();
        let rows_px = (y1 - y0) * width;
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        self.stack_pass(&self.pipelines.blend_layer_pipeline, &layer, &acc, rows_px.div_ceil(WORKGROUP_SIZE), "stack_blend");
        true
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BlendLayerParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) opacity: f32,
    pub(super) mode: u32,
    pub(super) y0: u32,
    pub(super) y1: u32,
    pub(super) _p0: u32,
    pub(super) _p1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct OpacityParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) value: f32,
    pub(super) _pad: u32,
}

/// A merge's top layer over the chain (#2758): its opacity and its blend
/// mode as `SHADER_BLEND_LAYER` numbers them (`BlendMode as u32`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuMergeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) opacity: f32,
    pub(super) mode: u32,
}

pub(super) const SHADER_OPACITY: &str = gpu_shader!("composite/opacity.wgsl");

// Opposite direction: packed sRGB-u8 RGBA (4 bytes per pixel, one u32)
// → linear-f32 vec4 RGBA. Reads from `input: array<u32>`, writes to
// `output: array<vec4<f32>>`. Same binding layout as SHADER_LINEAR_TO_U8
// so it reuses the pipeline's 3-binding bind_group_layout. Powers the
// phase-4d GPU-resident decode → grade path: the decoder's output
// buffer is plugged in as `input`, the grade chain's `buf_a` is the
// `output`.
// One layer onto the accumulator, in place: `klipp_effects::composite::blend_layer_linear`
// transcribed, mode by mode, in the same arithmetic order, so the stack
// on the GPU is the CPU stack to the level. Mode 13 is a plain lerp of
// every channel, an adjustment layer's opacity. Rows `y0..y1` only.
pub(super) const SHADER_BLEND_LAYER: &str = gpu_shader!("composite/blend_layer.wgsl");

// A merge's picture over the chain's (#2758): `layer_mixer_compose` for
// a top layer over its base, the blended colour mixed over the base by
// the opacity times the top's alpha, the base's alpha kept.
pub(super) const SHADER_MERGE_OVER: &str = gpu_shader!("composite/merge_over.wgsl");

// The composite's last step: colour times alpha, alpha to one.
pub(super) const SHADER_PREMULTIPLY: &str = gpu_shader!("composite/premultiply.wgsl");

pub(super) const SHADER_BLEND_MIX: &str = gpu_shader!("composite/blend_mix.wgsl");

pub(super) const SHADER_LAYER_OVER: &str = gpu_shader!("composite/layer_over.wgsl");
