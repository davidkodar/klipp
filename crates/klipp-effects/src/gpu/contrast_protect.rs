//! Contrast protection for titles on the GPU (#2734): the twin of
//! [`crate::protect::protect_backdrop`], run on the accumulator just
//! before a protected title is blended onto it.
use super::*;
use crate::protect;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProtectParams {
    width: u32,
    height: u32,
    op: u32,
    scale: u32,
    small_w: u32,
    small_h: u32,
    radius: u32,
    mode: u32,
    /// The working space's luminance weights and the layer's opacity.
    luma_opacity: [f32; 4],
}

impl GpuEffectsPipeline {
    /// Protects the text of the title in `slot` (#2734): darkens or
    /// lightens the picture laid so far round it, as
    /// [`protect::protect_backdrop`] does, by `mode` (1 auto, 2 always;
    /// 0 does nothing). `opacity` is the layer's as it will be blended,
    /// `luma` the working space's weights. Run just before the slot is
    /// blended; `false` when the pass could not run.
    pub fn stack_contrast_protect(&mut self, slot: usize, opacity: f32, mode: u32, luma: [f32; 3]) -> bool {
        if mode == 0 || opacity <= 0.001 {
            return true;
        }
        if !self.stack_ensure_acc() || slot >= self.stack_layers.len() {
            return false;
        }
        let (fw, fh) = self.stack_dims;
        let scale = protect::SCALE;
        let (sw, sh) = (fw.div_ceil(scale), fh.div_ceil(scale));
        let small = u64::from(sw) * u64::from(sh);
        if self.contrast_protect_scratch.as_ref().is_none_or(|(_, _, n)| *n < small) {
            let make = |label| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: small * 16, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
            self.contrast_protect_scratch = Some((make("contrast protect measure"), make("contrast protect blur"), small));
        }
        let params = |op: u32| ProtectParams {
            width: fw, height: fh, op, scale, small_w: sw, small_h: sh,
            radius: protect::radius_cells(fw, fh), mode,
            luma_opacity: [luma[0], luma[1], luma[2], opacity],
        };
        let acc = self.stack_acc.clone().unwrap();
        let layer = self.stack_layers[slot].clone();
        let (measure, blur, _) = self.contrast_protect_scratch.as_ref().unwrap();
        let small_groups = (small as u32).div_ceil(WORKGROUP_SIZE);
        let pass = |this: &Self, op: u32, read: &wgpu::Buffer, rw: &wgpu::Buffer, groups: u32| {
            this.write_params(&params(op));
            this.stack_pass(&this.pipelines.contrast_protect_pipeline, read, rw, groups, "contrast protect");
        };
        pass(self, 0, &acc, measure, small_groups);
        pass(self, 1, &layer, measure, small_groups);
        for _ in 0..protect::BLUR_ROUNDS {
            pass(self, 2, measure, blur, small_groups);
            pass(self, 3, blur, measure, small_groups);
        }
        pass(self, 4, measure, &acc, (u64::from(fw) * u64::from(fh)).div_ceil(u64::from(WORKGROUP_SIZE)) as u32);
        true
    }
}

pub(super) const SHADER_CONTRAST_PROTECT: &str = gpu_shader!("colour/contrast_protect.wgsl");
