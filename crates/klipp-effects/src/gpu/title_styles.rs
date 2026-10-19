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

/// Face inputs independent of the coverage used by its style stack.
#[derive(Clone, Copy, Debug)]
pub struct TitleFace {
    pub alpha: f32,
    pub outline_cutoff: i32,
    pub write_cutoff: i32,
    pub backing: Option<([u32; 4], [u8; 4])>,
}
impl Default for TitleFace {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            outline_cutoff: i32::MAX,
            write_cutoff: i32::MAX,
            backing: None,
        }
    }
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
pub(super) fn inverse(m: [[f32; 4]; 3]) -> Option<[[f32; 4]; 3]> {
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
    /// Rasterize retained outline edges at the evaluated glyph size. Only
    /// placement parameters change with size; no CPU glyph bitmap is produced.
    pub fn stack_title_outline(
        &mut self,
        slot: usize,
        id: u64,
        edges: &[[f32; 4]],
        origin: [i32; 2],
        size: [u32; 2],
        placement: [f32; 4],
        color: [u8; 4],
    ) -> bool {
        let [w, h] = size;
        let count = u64::from(w) * u64::from(h);
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || count == 0
            || edges.len() < 65
            || edges.len() > 65536
            || placement[0] <= 0.0
            || placement.iter().any(|x| !x.is_finite())
            || count.div_ceil(u64::from(WORKGROUP_SIZE))
                > u64::from(self.device.limits().max_compute_workgroups_per_dimension)
        {
            return false;
        }
        if edges.iter().flatten().any(|v| !v.is_finite())
            || edges[0][1] <= 0.0
            || edges[0][2] != 64.0
        {
            return false;
        }
        if edges[0][3] != 0.0 && edges[0][3] != 1.0 {
            return false;
        }
        let stride = if edges[0][3] == 1.0 { 3.0 } else { 1.0 };
        let segments = if edges[0][3] == 1.0 { 65 } else { 1 };
        let mut max_bin = 0u64;
        for bin in &edges[1..65] {
            if bin[0] < 65.0
                || bin[1] < 0.0
                || bin[0].fract() != 0.0
                || bin[1].fract() != 0.0
                || bin[0] + stride * bin[1] > edges.len() as f32
            {
                return false;
            }
            if stride == 3.0 {
                for i in 0..bin[1] as usize {
                    let degree = edges[bin[0] as usize + 3 * i + 2][0];
                    if degree != 1.0 && degree != 2.0 && degree != 3.0 {
                        return false;
                    }
                }
            }
            max_bin = max_bin.max(bin[1] as u64);
        }
        // Bounds even a pathological outline with many full-height edges.
        let bins_per_pixel = ((64.0 / (edges[0][1] * placement[0])).ceil() as u64)
            .saturating_add(1)
            .min(64);
        if count
            .saturating_mul(max_bin)
            .saturating_mul(bins_per_pixel)
            .saturating_mul(segments)
            > 64 * 1024 * 1024
        {
            return false;
        }
        let source = if let Some(i) = self.title_outlines.iter().position(|(key, _)| *key == id) {
            self.title_outlines.remove(i)
        } else {
            let bytes = bytemuck::cast_slice(edges);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title glyph outline"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.upload(&buffer, 0, bytes);
            (id, buffer)
        };
        let linear = |x: u8| {
            let x = x as f32 / 255.0;
            if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        };
        let rgb = [linear(color[0]), linear(color[1]), linear(color[2])];
        let working = self
            .authored_to_working
            .map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]);
        self.write_params(&StyleParams {
            width: w,
            height: h,
            frame_width: self.stack_dims.0,
            op: 14,
            x0: edges.len() as u32,
            y0: self.stack_dims.1,
            offset_x: origin[0],
            offset_y: origin[1],
            color: [working[0], working[1], working[2], 1.0],
            matrix: [placement, [0.0; 4], [0.0; 4]],
        });
        self.stack_pass(
            &self.title_style_pipeline,
            &source.1,
            &self.stack_layers[slot],
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title outline coverage",
        );
        while self.title_outlines.len() >= 512
            || self
                .title_outlines
                .iter()
                .map(|(_, b)| b.size())
                .sum::<u64>()
                + source.1.size()
                > 32 << 20
        {
            self.title_outlines.remove(0);
        }
        self.title_outlines.push(source);
        true
    }

    /// Solid coverage for underline/strikethrough geometry.
    pub fn stack_title_stripe(&mut self, slot: usize, rect: [u32; 4], color: [u8; 4]) -> bool {
        let [x, y, w, h] = rect;
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || w == 0
            || h == 0
            || x.checked_add(w).is_none_or(|v| v > self.stack_dims.0)
            || y.checked_add(h).is_none_or(|v| v > self.stack_dims.1)
        {
            return false;
        }
        let Some(input) = self.buf_a.as_ref() else {
            return false;
        };
        let linear = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let rgb = [linear(color[0]), linear(color[1]), linear(color[2])];
        let rgb = self
            .authored_to_working
            .map(|m| m[0] * rgb[0] + m[1] * rgb[1] + m[2] * rgb[2]);
        self.write_params(&StyleParams {
            width: w,
            height: h,
            frame_width: self.stack_dims.0,
            op: 15,
            x0: x,
            y0: y,
            offset_x: 0,
            offset_y: 0,
            color: [rgb[0], rgb[1], rgb[2], 1.0],
            matrix: [[0.0; 4]; 3],
        });
        self.stack_pass(
            &self.title_style_pipeline,
            input,
            &self.stack_layers[slot],
            (w * h).div_ceil(WORKGROUP_SIZE),
            "title underline coverage",
        );
        true
    }

    /// Authored title chromatic split followed by deterministic stripe glitch.
    /// This is the title algorithm, not the similarly named clip effects.
    /// The decorated layer stays on the device through both operations.
    pub fn stack_title_post(
        &mut self,
        slot: usize,
        chromatic_px: f32,
        glitch: f32,
        time: f32,
    ) -> bool {
        let (w, h) = self.stack_dims;
        let count = u64::from(w) * u64::from(h);
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || count == 0
            || count * 16 > u64::from(self.device.limits().max_storage_buffer_binding_size)
            || !chromatic_px.is_finite()
            || !glitch.is_finite()
        {
            return false;
        }
        let Some(to_authored) = inverse(self.authored_to_working) else {
            return false;
        };
        let chromatic = chromatic_px.max(0.0).round().min(w as f32) as i32;
        if glitch > (i32::MAX / 64) as f32 {
            return false;
        }
        let max_offset = if glitch > 0.001 {
            (glitch * 30.0).round().max(1.0) as i32
        } else {
            0
        };
        if chromatic == 0 && max_offset == 0 {
            return true;
        }
        if self
            .title_post_scratch
            .as_ref()
            .is_none_or(|b| b.size() < count * 16)
        {
            self.title_post_scratch = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title post scratch"),
                size: count * 16,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let scratch = self.title_post_scratch.as_ref().unwrap();
        let layer = &self.stack_layers[slot];
        let params = StyleParams {
            width: w,
            height: h,
            frame_width: w,
            op: 9,
            x0: (time * 12.0) as u32,
            y0: 0,
            offset_x: chromatic,
            offset_y: max_offset,
            color: [0.0; 4],
            matrix: to_authored,
        };
        self.write_params(&params);
        self.stack_pass(
            &self.title_style_pipeline,
            layer,
            scratch,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title post",
        );
        self.write_params(&StyleParams {
            op: 7,
            x0: 0,
            offset_x: 0,
            offset_y: 0,
            matrix: self.authored_to_working,
            ..params
        });
        self.stack_pass(
            &self.title_style_pipeline,
            scratch,
            layer,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title post working space",
        );
        true
    }

    /// Rotate the decorated layer with the authored nearest-pixel sampler.
    /// Bounds are reduced on-device to preserve the CPU renderer's crop.
    pub fn stack_title_rotate(&mut self, slot: usize, pivot: [f32; 2], radians: f32) -> bool {
        let (w, h) = self.stack_dims;
        let count = u64::from(w) * u64::from(h);
        let bytes = (count + u64::from(h) + 1) * 16;
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || count == 0
            || !radians.is_finite()
            || pivot.iter().any(|v| !v.is_finite())
            || bytes > u64::from(self.device.limits().max_storage_buffer_binding_size)
            || count.div_ceil(u64::from(WORKGROUP_SIZE))
                > u64::from(self.device.limits().max_compute_workgroups_per_dimension)
        {
            return false;
        }
        if self
            .title_post_scratch
            .as_ref()
            .is_none_or(|b| b.size() < bytes)
        {
            self.title_post_scratch = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title rotation scratch"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let layer = &self.stack_layers[slot];
        let scratch = self.title_post_scratch.as_ref().unwrap();
        let (sin, cos) = radians.sin_cos();
        let params = StyleParams {
            width: w,
            height: h,
            frame_width: w,
            op: 19,
            x0: 0,
            y0: 0,
            offset_x: 0,
            offset_y: 0,
            color: [pivot[0], pivot[1], sin, cos],
            matrix: [[0.0; 4]; 3],
        };
        self.write_params(&params);
        self.stack_pass(
            &self.title_style_pipeline,
            layer,
            scratch,
            h.div_ceil(WORKGROUP_SIZE),
            "title rotation row bounds",
        );
        self.write_params(&StyleParams { op: 20, ..params });
        self.stack_pass(
            &self.title_style_pipeline,
            layer,
            scratch,
            1,
            "title rotation bounds",
        );
        self.write_params(&StyleParams { op: 21, ..params });
        self.stack_pass(
            &self.title_style_pipeline,
            scratch,
            layer,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title rotation",
        );
        true
    }

    /// Fade the fully decorated title object after its post-processing.
    pub fn stack_title_alpha(&mut self, slot: usize, edges: [f32; 4], opacity: f32) -> bool {
        let (w, h) = self.stack_dims;
        let count = u64::from(w) * u64::from(h);
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || count == 0
            || !opacity.is_finite()
            || edges.iter().any(|x| !x.is_finite())
            || count * 16 > u64::from(self.device.limits().max_storage_buffer_binding_size)
        {
            return false;
        }
        if self
            .title_post_scratch
            .as_ref()
            .is_none_or(|b| b.size() < count * 16)
        {
            self.title_post_scratch = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title fade scratch"),
                size: count * 16,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let layer = &self.stack_layers[slot];
        let scratch = self.title_post_scratch.as_ref().unwrap();
        self.write_params(&StyleParams {
            width: w,
            height: h,
            frame_width: w,
            op: 12,
            x0: 0,
            y0: 0,
            offset_x: 0,
            offset_y: 0,
            color: edges.map(|v| v.clamp(0.0, 0.5)),
            matrix: [[opacity.clamp(0.0, 1.0), 0.0, 0.0, 0.0], [0.0; 4], [0.0; 4]],
        });
        self.stack_pass(
            &self.title_style_pipeline,
            layer,
            scratch,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title edge fade",
        );
        let mut enc = self.encoder(Some("title fade copy"));
        enc.copy_buffer_to_buffer(scratch, 0, layer, 0, count * 16);
        self.submit(enc);
        true
    }

    /// Multiply an object's alpha by another object's authored mask.
    pub fn stack_title_mask(
        &mut self,
        source: usize,
        destination: usize,
        luma: bool,
        invert: bool,
    ) -> bool {
        let (w, h) = self.stack_dims;
        if self.is_device_lost()
            || source == destination
            || source >= self.stack_layers.len()
            || destination >= self.stack_layers.len()
        {
            return false;
        }
        let Some(to_authored) = inverse(self.authored_to_working) else {
            return false;
        };
        self.write_params(&StyleParams {
            width: w,
            height: h,
            frame_width: w,
            op: 13,
            x0: 0,
            y0: 0,
            offset_x: i32::from(luma),
            offset_y: i32::from(invert),
            color: [0.0; 4],
            matrix: to_authored,
        });
        self.stack_pass(
            &self.title_style_pipeline,
            &self.stack_layers[source],
            &self.stack_layers[destination],
            (w * h).div_ceil(WORKGROUP_SIZE),
            "title object mask",
        );
        true
    }

    /// Compose one title object in authored sRGB, preserving fractional alpha.
    pub fn stack_title_over(
        &mut self,
        source: usize,
        destination: usize,
        opacity: f32,
        mode: crate::BlendMode,
    ) -> bool {
        let (w, h) = self.stack_dims;
        let count = u64::from(w) * u64::from(h);
        if source == destination
            || source >= self.stack_layers.len()
            || destination >= self.stack_layers.len()
            || count == 0
            || !opacity.is_finite()
            || self.is_device_lost()
            || count * 16 > u64::from(self.device.limits().max_storage_buffer_binding_size)
        {
            return false;
        }
        let Some(to_authored) = inverse(self.authored_to_working) else {
            return false;
        };
        if self
            .title_post_scratch
            .as_ref()
            .is_none_or(|b| b.size() < count * 16)
        {
            self.title_post_scratch = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title scene scratch"),
                size: count * 16,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let params = StyleParams {
            width: w,
            height: h,
            frame_width: w,
            op: 10,
            x0: 0,
            y0: 0,
            offset_x: mode as i32,
            offset_y: 0,
            color: [0.0, 0.0, 0.0, opacity.clamp(0.0, 1.0)],
            matrix: to_authored,
        };
        let src = &self.stack_layers[source];
        let dst = &self.stack_layers[destination];
        let scratch = self.title_post_scratch.as_ref().unwrap();
        self.write_params(&params);
        self.stack_pass(
            &self.title_style_pipeline,
            src,
            dst,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title object over",
        );
        self.write_params(&StyleParams {
            op: 11,
            matrix: self.authored_to_working,
            ..params
        });
        self.stack_pass(
            &self.title_style_pipeline,
            dst,
            scratch,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title scene working space",
        );
        let mut enc = self.encoder(Some("title scene copy"));
        enc.copy_buffer_to_buffer(scratch, 0, dst, 0, count * 16);
        self.submit(enc);
        true
    }

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
        self.stack_title_styles_with_face(slot, roi, styles, TitleFace::default())
    }

    /// As `stack_title_styles`, with face opacity separate from its coverage:
    /// translucent text fill must not weaken its shadow or outline mask.
    pub fn stack_title_styles_with_face(
        &mut self,
        slot: usize,
        roi: [u32; 4],
        styles: &[TitleStyle],
        face: TitleFace,
    ) -> bool {
        let TitleFace {
            alpha: fill_alpha,
            outline_cutoff,
            write_cutoff,
            backing,
        } = face;
        if !fill_alpha.is_finite() {
            return false;
        }
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
        pass(1, layer, &s.mask, [write_cutoff, 0], [0.0; 4], to_authored);
        let mut enc = self.encoder(Some("clear title style accumulation"));
        enc.clear_buffer(&s.composite, 0, None);
        self.submit(enc);
        if let Some((rect, color)) = backing {
            pass(
                17,
                layer,
                &s.composite,
                [0, 0],
                color.map(|v| v as f32 / 255.0),
                [rect.map(|v| v as f32), [0.0; 4], [0.0; 4]],
            );
        }
        for style in styles {
            let (color, intensity) = match *style {
                TitleStyle::Face => {
                    pass(
                        6,
                        layer,
                        &s.composite,
                        [write_cutoff, 0],
                        [0.0, 0.0, 0.0, fill_alpha],
                        to_authored,
                    );
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
                    pass(
                        3,
                        &s.mask,
                        &s.work,
                        [outline_cutoff, 0],
                        [0.0; 4],
                        to_authored,
                    );
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
        // A posed glyph may extend outside the authored mask box. The CPU
        // clips coverage to that box before styles; remove raw seed pixels
        // outside it after the face has been consumed by the style passes.
        let mut enc = self.encoder(Some("clear title coverage seed"));
        enc.clear_buffer(layer, 0, None);
        self.submit(enc);
        if let Some((rect, color)) = backing {
            self.write_params(&StyleParams {
                width: fw,
                height: fh,
                frame_width: fw,
                op: 18,
                x0: rect[0],
                y0: rect[1],
                offset_x: rect[2] as i32,
                offset_y: rect[3] as i32,
                color: color.map(|v| v as f32 / 255.0),
                matrix: self.authored_to_working,
            });
            self.stack_pass(
                &self.title_style_pipeline,
                &s.composite,
                layer,
                (fw * fh).div_ceil(WORKGROUP_SIZE),
                "title backing outside mask",
            );
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
