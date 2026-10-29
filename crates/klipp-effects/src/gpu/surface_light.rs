//! A pinned layer taking its surface's light and texture on the GPU
//! (#2751): the twin of [`crate::surface_light::take_surface`], run on the
//! layer just before it is blended onto the accumulator under it.
use super::*;
use crate::surface_light as light;

/// The pass's buffers: the grid, its blur, the row sums, a frame of
/// factors, and the cell and pixel counts they hold.
pub(super) struct Scratch {
    grid: wgpu::Buffer,
    blur: wgpu::Buffer,
    rows: wgpu::Buffer,
    frame: wgpu::Buffer,
    cells: u64,
    pixels: u64,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceParams {
    width: u32,
    height: u32,
    op: u32,
    scale: u32,
    small_w: u32,
    small_h: u32,
    radius: u32,
    shade: f32,
    texture: f32,
    _pad: [u32; 3],
    /// The working space's luminance weights.
    luma: [f32; 4],
}

impl GpuEffectsPipeline {
    /// The layer in `slot` given the light and texture of what is laid
    /// under it (#2751), as [`light::take_surface`] gives it: `shade` and
    /// `texture` 0 to 1, `luma` the working space's weights. Run just
    /// before the slot is blended; `false` when the pass could not run.
    pub fn stack_take_surface(&mut self, slot: usize, shade: f32, texture: f32, luma: [f32; 3]) -> bool {
        if shade <= 0.0 && texture <= 0.0 {
            return true;
        }
        if !self.stack_ensure_acc() || slot >= self.stack_layers.len() {
            return false;
        }
        let (fw, fh) = self.stack_dims;
        let scale = light::SCALE;
        let (sw, sh) = (fw.div_ceil(scale), fh.div_ceil(scale));
        let (cells, pixels) = (u64::from(sw) * u64::from(sh), u64::from(fw) * u64::from(fh));
        if self.surface_light_scratch.as_ref().is_none_or(|s| s.cells < cells || s.pixels < pixels) {
            let make = |label, n: u64| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: n.max(1) * 16, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
            self.surface_light_scratch = Some(Scratch {
                grid: make("surface light grid", cells),
                blur: make("surface light blur", cells),
                rows: make("surface light rows", u64::from(sh)),
                frame: make("surface light frame", pixels),
                cells,
                pixels,
            });
        }
        let params = |op: u32| SurfaceParams {
            width: fw, height: fh, op, scale, small_w: sw, small_h: sh,
            radius: light::radius_cells(fw, fh), shade, texture, _pad: [0; 3],
            luma: [luma[0], luma[1], luma[2], 0.0],
        };
        let acc = self.stack_acc.clone().unwrap();
        let layer = self.stack_layers[slot].clone();
        let s = self.surface_light_scratch.as_ref().unwrap();
        let groups = |n: u64| n.div_ceil(u64::from(WORKGROUP_SIZE)) as u32;
        let pass = |this: &Self, op: u32, read: &wgpu::Buffer, rw: &wgpu::Buffer, n: u64| {
            this.write_params(&params(op));
            this.stack_pass(&this.pipelines.surface_light_pipeline, read, rw, groups(n), "surface light");
        };
        pass(self, 0, &acc, &s.grid, cells);
        pass(self, 1, &layer, &s.grid, cells);
        for _ in 0..light::BLUR_ROUNDS {
            pass(self, 2, &s.grid, &s.blur, cells);
            pass(self, 3, &s.blur, &s.grid, cells);
        }
        pass(self, 4, &s.grid, &s.rows, u64::from(sh));
        pass(self, 5, &s.rows, &s.blur, 1);
        pass(self, 6, &s.blur, &s.grid, cells);
        pass(self, 7, &acc, &s.frame, pixels);
        pass(self, 8, &s.grid, &s.frame, pixels);
        pass(self, 9, &s.frame, &layer, pixels);
        true
    }
}

pub(super) const SHADER_SURFACE_LIGHT: &str = gpu_shader!("light/surface_light.wgsl");
