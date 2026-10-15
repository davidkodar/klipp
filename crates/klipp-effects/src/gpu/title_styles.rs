//! GPU title finishing, with bounded ROI scratch reused between layers.
use super::*;

/// Evaluated styles in authored order (behind styles, face, then inner styles).
#[derive(Clone, Copy, Debug)]
pub enum TitleStyle {
    Glow {
        radius: f32,
        color: [u8; 4],
        intensity: f32,
    },
    Shadow {
        offset: [i32; 2],
        radius: f32,
        color: [u8; 4],
    },
    Outline {
        radius: f32,
        color: [u8; 4],
    },
    Face,
    InnerShadow {
        offset: [i32; 2],
        radius: f32,
        color: [u8; 4],
    },
}

pub(super) struct StyleScratch {
    count: u64,
    mask: wgpu::Buffer,
    work: wgpu::Buffer,
    blur: wgpu::Buffer,
    composite: wgpu::Buffer,
}
impl StyleScratch {
    fn new(device: &wgpu::Device, count: u64) -> Self {
        let buffer = |channels| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title style ROI"),
                size: count * channels * 4,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            count,
            mask: buffer(1),
            work: buffer(1),
            blur: buffer(1),
            composite: buffer(4),
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StyleParams {
    width: u32,
    height: u32,
    frame_width: u32,
    op: u32,
    x0: u32,
    y0: u32,
    offset_x: i32,
    offset_y: i32,
    color: [f32; 4],
    matrix: [[f32; 4]; 3],
}
fn inverse(m: [[f32; 4]; 3]) -> Option<[[f32; 4]; 3]> {
    let [a, b, c] = m;
    let cross = |x: [f32; 4], y: [f32; 4]| {
        [
            x[1] * y[2] - x[2] * y[1],
            x[2] * y[0] - x[0] * y[2],
            x[0] * y[1] - x[1] * y[0],
        ]
    };
    let x = cross(b, c);
    let y = cross(c, a);
    let z = cross(a, b);
    let d = a[0] * x[0] + a[1] * x[1] + a[2] * x[2];
    if !d.is_finite() || d.abs() < 1e-8 {
        return None;
    }
    Some(std::array::from_fn(|i| [x[i] / d, y[i] / d, z[i] / d, 0.0]))
}
impl GpuEffectsPipeline {
    /// Finish a plain opaque face already seeded in a stack layer. Its alpha
    /// must be the original coverage, not opacity-modulated fill. `roi` is the
    /// original padded mask box, including zero padding needed by inner shadows.
    /// The caller supplies the CPU title stack's evaluated and sorted order.
    /// Unsupported inputs return false before changing the layer.
    pub fn stack_title_styles(
        &mut self,
        slot: usize,
        roi: [u32; 4],
        styles: &[TitleStyle],
    ) -> bool {
        let [x0, y0, w, h] = roi;
        let (fw, fh) = self.stack_dims;
        let count = u64::from(w) * u64::from(h);
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || count == 0
            || x0.checked_add(w).is_none_or(|x| x > fw)
            || y0.checked_add(h).is_none_or(|y| y > fh)
            || count * 16 > u64::from(self.device.limits().max_storage_buffer_binding_size)
            || count.div_ceil(u64::from(WORKGROUP_SIZE))
                > u64::from(self.device.limits().max_compute_workgroups_per_dimension)
            || styles.len() > 256
            || styles
                .iter()
                .filter(|s| matches!(s, TitleStyle::Face))
                .count()
                != 1
        {
            return false;
        }
        for style in styles {
            if let TitleStyle::Shadow { offset, .. } | TitleStyle::InnerShadow { offset, .. } =
                style
            {
                if offset.iter().zip([w, h]).any(|(&o, n)| {
                    0i32.checked_sub(o).is_none() || ((n - 1) as i32).checked_sub(o).is_none()
                }) {
                    return false;
                }
            }
            let (radius, intensity) = match *style {
                TitleStyle::Glow {
                    radius, intensity, ..
                } => (radius, intensity),
                TitleStyle::Shadow { radius, .. }
                | TitleStyle::Outline { radius, .. }
                | TitleStyle::InnerShadow { radius, .. } => (radius, 1.0),
                TitleStyle::Face => (0.0, 1.0),
            };
            // The integer dilation kernel squares offsets in signed 32 bits.
            if !radius.is_finite()
                || radius < 0.0
                || radius > 32767.0
                || !intensity.is_finite()
                || intensity < 0.0
            {
                return false;
            }
        }
        let Some(to_authored) = inverse(self.authored_to_working) else {
            return false;
        };
        if self
            .title_style_scratch
            .as_ref()
            .is_none_or(|s| s.count < count)
        {
            self.title_style_scratch = Some(StyleScratch::new(&self.device, count));
        }
        let s = self.title_style_scratch.as_ref().unwrap();
        let layer = &self.stack_layers[slot];
        let base = StyleParams {
            width: w,
            height: h,
            frame_width: fw,
            op: 0,
            x0,
            y0,
            offset_x: 0,
            offset_y: 0,
            color: [0.0; 4],
            matrix: to_authored,
        };
        let pass = |op,
                    src: &wgpu::Buffer,
                    dst: &wgpu::Buffer,
                    offset: [i32; 2],
                    color: [f32; 4],
                    matrix| {
            self.write_params(&StyleParams {
                op,
                offset_x: offset[0],
                offset_y: offset[1],
                color,
                matrix,
                ..base
            });
            self.stack_pass(
                &self.title_style_pipeline,
                src,
                dst,
                (count as u32).div_ceil(WORKGROUP_SIZE),
                "title style",
            );
        };
        pass(1, layer, &s.mask, [0, 0], [0.0; 4], to_authored);
        let mut enc = self.encoder(Some("clear title style accumulation"));
        enc.clear_buffer(&s.composite, 0, None);
        self.submit(enc);
        for style in styles {
            let (color, intensity) = match *style {
                TitleStyle::Face => {
                    pass(6, layer, &s.composite, [0, 0], [0.0; 4], to_authored);
                    continue;
                }
                TitleStyle::Glow {
                    radius,
                    color,
                    intensity,
                } => {
                    if !self.record_alpha_blur(&s.mask, &s.work, &s.blur, w, h, radius) {
                        return false;
                    }
                    (color, intensity)
                }
                TitleStyle::Outline { radius, color } => {
                    if !self.record_alpha_dilate(&s.mask, &s.work, w, h, radius) {
                        return false;
                    }
                    pass(3, &s.mask, &s.work, [0, 0], [0.0; 4], to_authored);
                    (color, 1.0)
                }
                TitleStyle::Shadow {
                    offset,
                    radius,
                    color,
                }
                | TitleStyle::InnerShadow {
                    offset,
                    radius,
                    color,
                } => {
                    let inner = matches!(style, TitleStyle::InnerShadow { .. });
                    pass(
                        if inner { 8 } else { 2 },
                        &s.mask,
                        &s.work,
                        offset,
                        [0.0; 4],
                        to_authored,
                    );
                    if !self.record_alpha_blur(&s.work, &s.work, &s.blur, w, h, radius) {
                        return false;
                    }
                    if inner {
                        pass(4, &s.mask, &s.work, [0, 0], [0.0; 4], to_authored);
                    }
                    (color, 1.0)
                }
            };
            let mut c = color.map(|x| f32::from(x) / 255.0);
            c[3] *= intensity;
            pass(5, &s.work, &s.composite, [0, 0], c, to_authored);
        }
        pass(
            7,
            &s.composite,
            layer,
            [0, 0],
            [0.0; 4],
            self.authored_to_working,
        );
        true
    }
    /// Resident finishing scratch only; excludes shared stack and source caches.
    pub fn title_style_scratch_bytes(&self) -> u64 {
        self.title_style_scratch
            .as_ref()
            .map_or(0, |s| s.count * 28)
    }
}
