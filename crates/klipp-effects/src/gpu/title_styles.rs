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

/// A vector path's geometry for [`GpuEffectsPipeline::stack_title_path`]:
/// the CPU raster's padded box in frame pixels, its closed contours' edges
/// and revealed stroke segments (`[a.x, a.y, b.x, b.y]`, offset to that box
/// as the CPU offsets them), the stroke's half width, the pose's colours
/// (0..1) and its glow (radius in pixels, strength).
#[derive(Clone, Debug, Default)]
pub struct TitlePath {
    pub bounds: [u32; 4],
    pub fill_edges: Vec<[f32; 4]>,
    pub stroke_segments: Vec<[f32; 4]>,
    pub half: f32,
    pub fill: [f32; 4],
    pub stroke: [f32; 4],
    pub glow: Option<(f32, f32)>,
    /// A gradient fill (#2208): the stops and geometry as a rectangle's
    /// gradient takes them, and how a frame pixel maps to the object's box
    /// (pivot x, y, cosine, sine, box size, anchor fractions).
    pub gradient: Option<(TitleFill, [f32; 8])>,
}

/// Fill evaluated in authored pixel coordinates, independently of style coverage.
#[derive(Clone, Debug)]
pub enum TitleFill {
    Pair {
        top: [u8; 4],
        bottom: [u8; 4],
    },
    Gradient {
        geometry: [f32; 4],
        stops: Vec<(f32, [u8; 4])>,
    },
    Pattern {
        colors: [[u8; 4]; 2],
        mode: u32,
        spacing: f32,
    },
    /// A metal face (#2706), its material resolved to the numbers the
    /// shader takes.
    Metal(TitleMetal),
}

/// A metal face's material as the GPU draws it (#2706): the caller
/// resolves a title's material to directions, linear colours and pixel
/// sizes. Directions point towards the light, in the frame's axes (x
/// right, y down, z into the frame).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleMetal {
    /// Reflectance facing the viewer, linear RGB.
    pub tint: [f32; 3],
    pub roughness: f32,
    pub edge: f32,
    pub highlights: f32,
    pub glints: f32,
    pub anisotropy: f32,
    pub anisotropy_radians: f32,
    /// The bevel's width in frame pixels, and how steep it is (0..1).
    pub bevel_px: f32,
    pub depth: f32,
    pub key: ([f32; 3], f32, [f32; 3]),
    pub rim: ([f32; 3], f32, [f32; 3]),
    pub studio: f32,
    pub sky: f32,
    pub sky_tint: [f32; 3],
    /// Which points glint, and the size of the cells they are picked in
    /// (frame pixels).
    pub seed: f32,
    pub glint_cell: f32,
}

/// A glyph's pixel-space pose after source scaling.
pub struct TitleGlyphPose {
    pub alpha: f32,
    pub clip_y: [i32; 2],
    pub source_size: [u32; 2],
    pub blur: f32,
    pub radians: f32,
    pub face: Option<[u32; 4]>,
    pub path: bool,
}
/// Cached texture and its authored UV transform, applied after title styles.
pub struct TitleTexture<'a> {
    pub id: u64,
    pub texels: &'a [u32],
    pub size: [u32; 2],
    pub bounds: [f32; 4],
    pub scale: f32,
    pub offset: [f32; 2],
    pub radians: f32,
    pub opacity: f32,
    pub tint: [u8; 4],
    pub mode: u32,
}
/// Face inputs independent of the coverage used by its style stack.
#[derive(Clone, Debug)]
pub struct TitleFace {
    pub alpha: f32,
    pub outline_cutoff: i32,
    pub write_cutoff: i32,
    pub backing: Option<([u32; 4], [u8; 4])>,
    pub fill: Option<TitleFill>,
    pub rich_slot: Option<usize>,
    /// A layer holding the face's own coverage, when it differs from the
    /// slot's: colour glyphs leave the face but still cast shadow, glow,
    /// outline and inner shadow, as on the CPU.
    pub face_slot: Option<usize>,
    /// A layer holding colour glyphs, laid over the face before the inner
    /// shadows.
    pub colour_slot: Option<usize>,
    pub bounds: [f32; 4],
}
impl Default for TitleFace {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            outline_cutoff: i32::MAX,
            write_cutoff: i32::MAX,
            backing: None,
            fill: None,
            rich_slot: None,
            face_slot: None,
            colour_slot: None,
            bounds: [0.0; 4],
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
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FillParams {
    base: StyleParams,
    // Two (position bits, packed RGBA) stops per aligned uniform vector.
    stops: [[u32; 4]; 8],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextureParams {
    base: StyleParams,
    to_authored: [[f32; 4]; 3],
    uv: [f32; 4],
    extra: [f32; 4],
    tint: [f32; 4],
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
        pose: TitleGlyphPose,
    ) -> bool {
        let TitleGlyphPose {
            alpha,
            clip_y,
            source_size,
            blur,
            radians,
            face, path,
        } = pose;
        let [w, h] = size;
        let count = u64::from(w) * u64::from(h);
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || count == 0
            || !alpha.is_finite()
            || !blur.is_finite()
            || !radians.is_finite()
            || source_size.contains(&0)
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
            .max(u64::from(source_size[0]) * u64::from(source_size[1]))
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
        let ink = if face.is_some() {
            color.map(|v| v as f32 / 255.0)
        } else {
            [working[0], working[1], working[2], 1.0]
        };
        if blur >= 0.5 || radians != 0.0 || path {
            if !self.pose_outline_coverage(
                slot,
                &source.1,
                origin,
                size,
                source_size,
                placement,
                ink,
                alpha,
                clip_y,
                blur,
                radians,
                face, path,
            ) {
                return false;
            }
        } else {
            let params = StyleParams {
                width: w,
                height: h,
                frame_width: self.stack_dims.0,
                op: 14,
                x0: u32::from(face.is_some()),
                y0: self.stack_dims.1,
                offset_x: origin[0],
                offset_y: origin[1],
                color: ink,
                matrix: [
                    placement,
                    [
                        clip_y[0] as f32,
                        clip_y[1] as f32,
                        alpha.clamp(0.0, 1.0),
                        0.0,
                    ],
                    face.unwrap_or([0; 4]).map(|v| v as f32),
                ],
            };
            self.write_params(&params);
            self.stack_pass(
                &self.title_style_pipeline,
                &source.1,
                &self.stack_layers[slot],
                (count as u32).div_ceil(WORKGROUP_SIZE),
                "title outline coverage",
            );
        }
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

    #[allow(clippy::too_many_arguments)]
    fn pose_outline_coverage(
        &mut self,
        slot: usize,
        outline: &wgpu::Buffer,
        origin: [i32; 2],
        size: [u32; 2],
        raw: [u32; 2],
        placement: [f32; 4],
        color: [f32; 4],
        alpha: f32,
        clip: [i32; 2],
        sigma: f32,
        radians: f32,
        face: Option<[u32; 4]>,
        path: bool,
    ) -> bool {
        let pad = if sigma >= 0.5 {
            (sigma * 2.5).ceil().max(1.0) as u32
        } else {
            0
        };
        let Some(w) = raw[0].checked_add(pad.saturating_mul(2)) else {
            return false;
        };
        let Some(h) = raw[1].checked_add(pad.saturating_mul(2)) else {
            return false;
        };
        let count = u64::from(w) * u64::from(h);
        if count * 16 > u64::from(self.device.limits().max_storage_buffer_binding_size)
            || count.div_ceil(u64::from(WORKGROUP_SIZE))
                > u64::from(self.device.limits().max_compute_workgroups_per_dimension)
        {
            return false;
        }
        if self
            .title_style_scratch
            .as_ref()
            .is_none_or(|s| s.count < count)
        {
            self.title_style_scratch = Some(StyleScratch::new(&self.device, count));
        }
        let s = self.title_style_scratch.as_ref().unwrap();
        let base = StyleParams {
            width: raw[0],
            height: raw[1],
            frame_width: self.stack_dims.0,
            op: 25,
            x0: u32::from(face.is_some()),
            y0: self.stack_dims.1,
            offset_x: origin[0],
            offset_y: origin[1],
            color,
            matrix: [
                placement,
                [clip[0] as f32, clip[1] as f32, alpha, 0.0],
                face.unwrap_or([0; 4]).map(|v| v as f32),
            ],
        };
        self.write_params(&base);
        self.stack_pass(
            &self.title_style_pipeline,
            outline,
            &s.mask,
            (raw[0] * raw[1]).div_ceil(WORKGROUP_SIZE),
            "title glyph coverage",
        );
        self.write_params(&StyleParams {
            width: w,
            height: h,
            op: 26,
            x0: pad,
            matrix: [[raw[0] as f32, raw[1] as f32, 0.0, 0.0], [0.0; 4], [0.0; 4]],
            ..base
        });
        self.stack_pass(
            &self.title_style_pipeline,
            &s.mask,
            &s.work,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title glyph padding",
        );
        let mut current = &s.work;
        let mut next = &s.blur;
        if (0.5..=8.0).contains(&sigma) {
            let inv = 1.0 / (2.0 * sigma * sigma);
            let weights: Vec<_> = (0..=2 * pad)
                .map(|i| {
                    let x = i as f32 - pad as f32;
                    (-x * x * inv).exp()
                })
                .collect();
            let sum: f32 = weights.iter().sum();
            let mut kernel = [[0u32; 4]; 8];
            for i in 0..=pad as usize {
                kernel[i / 4][i % 4] = (weights[pad as usize + i] / sum).to_bits();
            }
            for axis in 0..2 {
                self.write_params(&FillParams {
                    base: StyleParams {
                        width: w,
                        height: h,
                        op: 27,
                        offset_x: pad as i32,
                        offset_y: axis,
                        ..base
                    },
                    stops: kernel,
                });
                self.stack_pass(
                    &self.title_style_pipeline,
                    current,
                    next,
                    (count as u32).div_ceil(WORKGROUP_SIZE),
                    "title glyph Gaussian",
                );
                std::mem::swap(&mut current, &mut next);
            }
        } else if sigma > 8.0 {
            let ideal = (4.0 * sigma * sigma + 1.0).sqrt();
            let mut low = ideal.floor() as i32;
            if low % 2 == 0 {
                low -= 1;
            }
            let l = low as f32;
            let m = ((12.0 * sigma * sigma - 3.0 * l * l - 12.0 * l - 9.0) / (-4.0 * l - 4.0))
                .round() as i32;
            for axis in 0..2 {
                for j in 0..3 {
                    let radius = ((if j < m { low } else { low + 2 }).max(1) - 1) / 2;
                    self.write_params(&StyleParams {
                        width: w,
                        height: h,
                        op: 28,
                        offset_x: radius,
                        offset_y: axis,
                        x0: u32::from(axis == 1 && j == 2),
                        ..base
                    });
                    self.stack_pass(
                        &self.title_style_pipeline,
                        current,
                        next,
                        (if axis == 0 { h } else { w }).div_ceil(WORKGROUP_SIZE),
                        "title glyph box blur",
                    );
                    std::mem::swap(&mut current, &mut next);
                }
            }
        }
        let (sin, cos) = radians.sin_cos();
        self.write_params(&StyleParams {
            width: size[0],
            height: size[1],
            op: 29,
            matrix: [
                [w as f32, h as f32, cos, sin],
                [
                    clip[0] as f32,
                    clip[1] as f32,
                    alpha.clamp(0.0, 1.0),
                    if path { 2.0 } else if radians != 0.0 { 1.0 } else { 0.0 },
                ],
                face.unwrap_or([0; 4]).map(|v| v as f32),
            ],
            ..base
        });
        self.stack_pass(
            &self.title_style_pipeline,
            current,
            &self.stack_layers[slot],
            (size[0] * size[1]).div_ceil(WORKGROUP_SIZE),
            "title posed glyph",
        );
        true
    }

    /// Solid coverage for underline/strikethrough geometry.
    pub fn stack_title_stripe(
        &mut self,
        slot: usize,
        rect: [u32; 4],
        color: [u8; 4],
        face: bool,
    ) -> bool {
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
            offset_x: i32::from(face),
            offset_y: 0,
            color: if face {
                color.map(|v| v as f32 / 255.0)
            } else {
                [rgb[0], rgb[1], rgb[2], 1.0]
            },
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

    /// Apply a cached texture to the quantized decorated source. The coverage
    /// mask survives styling so texture alpha matches the authored operation.
    pub fn stack_title_texture(
        &mut self,
        slot: usize,
        roi: [u32; 4],
        texture: TitleTexture<'_>,
    ) -> bool {
        let (fw, fh) = self.stack_dims;
        let [x0, y0, w, h] = roi;
        let count = u64::from(w) * u64::from(h);
        let frame = count;
        let [tw, th] = texture.size;
        let texbytes = u64::from(tw) * u64::from(th) * 8;
        let bytes = frame * 16 + count * 4 + texbytes;
        if self.is_device_lost()
            || slot >= self.stack_layers.len()
            || count == 0
            || tw == 0
            || th == 0
            || x0.checked_add(w).is_none_or(|x| x > fw)
            || y0.checked_add(h).is_none_or(|y| y > fh)
            || bytes > u64::from(self.device.limits().max_storage_buffer_binding_size)
            || texture.texels.len() as u64 * 4 != texbytes
            || texture.mode > 3
            || texture
                .bounds
                .iter()
                .chain(texture.offset.iter())
                .any(|v| !v.is_finite())
            || !texture.scale.is_finite()
            || !texture.radians.is_finite()
            || !texture.opacity.is_finite()
            || count.div_ceil(u64::from(WORKGROUP_SIZE))
                > u64::from(self.device.limits().max_compute_workgroups_per_dimension)
        {
            return false;
        }
        let Some(to_authored) = inverse(self.authored_to_working) else {
            return false;
        };
        if self
            .title_style_scratch
            .as_ref()
            .is_none_or(|s| s.count < count)
        {
            return false;
        }
        let pos = self.resident_sprite(texture.id, texture.texels);
        if self
            .title_post_scratch
            .as_ref()
            .is_none_or(|b| b.size() < bytes)
        {
            self.title_post_scratch = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title texture scratch"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let scratch = self.title_post_scratch.as_ref().unwrap();
        let layer = &self.stack_layers[slot];
        self.write_params(&StyleParams {
            width: w,
            height: h,
            frame_width: fw,
            op: 24,
            x0,
            y0,
            offset_x: 0,
            offset_y: 0,
            color: [0.0; 4],
            matrix: [[0.0; 4]; 3],
        });
        self.stack_pass(
            &self.title_style_pipeline,
            layer,
            scratch,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title texture source ROI",
        );
        let mut enc = self.encoder(Some("title texture inputs"));
        enc.copy_buffer_to_buffer(
            &self.title_style_scratch.as_ref().unwrap().mask,
            0,
            scratch,
            frame * 16,
            count * 4,
        );
        enc.copy_buffer_to_buffer(
            &self.stack_sprites[pos].buffer,
            0,
            scratch,
            frame * 16 + count * 4,
            texbytes,
        );
        self.submit(enc);
        let (sin, cos) = texture.radians.sin_cos();
        self.write_params(&TextureParams {
            base: StyleParams {
                width: w,
                height: h,
                frame_width: fw,
                op: 23,
                x0,
                y0,
                offset_x: tw as i32,
                offset_y: th as i32,
                color: texture.bounds,
                matrix: self.authored_to_working,
            },
            to_authored,
            uv: [
                if texture.scale.abs() < 1e-6 {
                    1.0
                } else {
                    1.0 / texture.scale
                },
                cos,
                sin,
                texture.opacity.clamp(0.0, 1.0),
            ],
            extra: [
                texture.offset[0] / tw as f32,
                texture.offset[1] / th as f32,
                texture.mode as f32,
                fh as f32,
            ],
            tint: texture.tint.map(|v| v as f32 / 255.0),
        });
        self.stack_pass(
            &self.title_style_pipeline,
            scratch,
            layer,
            (count as u32).div_ceil(WORKGROUP_SIZE),
            "title texture fill",
        );
        true
    }

    /// Fade the fully decorated title object after its post-processing.
    pub fn stack_title_alpha(&mut self, slot: usize, edges: [f32; 4], opacity: f32) -> bool {
        self.stack_title_fade(slot, edges, opacity, None)
    }

    /// Combined edge and focus fade, quantized once as in the authored layer.
    pub fn stack_title_fade(&mut self, slot: usize, edges: [f32; 4], opacity: f32, focus: Option<(bool, f32, f32)>) -> bool {
        self.stack_title_fade_impl(slot, edges, opacity, focus, false)
    }

    /// A painted byte layer posed through the CPU's half-float sprite before
    /// motion opacity, then rounded back to bytes. Edge fades follow separately.
    pub fn stack_title_motion_alpha(&mut self, slot: usize, opacity: f32) -> bool {
        self.stack_title_fade_impl(slot, [0.0; 4], opacity, None, true)
    }

    fn stack_title_fade_impl(&mut self, slot: usize, edges: [f32; 4], opacity: f32, focus: Option<(bool, f32, f32)>, sprite_alpha: bool) -> bool {
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
            matrix: [[opacity.clamp(0.0, 1.0), if sprite_alpha {1.0}else{0.0}, 0.0, 0.0], focus.map_or([0.0;4], |(vertical,band,edge)| [if vertical {2.0}else{1.0},band.clamp(0.0,0.999),edge.clamp(0.0,1.0),0.0]), [0.0; 4]],
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

    /// CPU title blur uses three premultiplied authored-colour box passes.
    /// Keep the original layer for the focus mix and reuse the temporary face slot.
    pub fn stack_title_focus_blur(&mut self, slot: usize, radius: f32, vertical: bool, band: f32) -> bool {
        if radius <= 0.5 { return true; }
        if !radius.is_finite() || !band.is_finite() { return false; }
        let Some(temp) = self.stack_title_object_slot(2) else { return false; };
        if temp == slot || !self.stack_title_alpha(slot, [0.0;4], 1.0) { return false; }
        let Some(to_authored) = inverse(self.authored_to_working) else { return false; };
        let (w,h) = self.stack_dims;
        let count = w*h;
        let source=&self.stack_layers[slot];
        let blurred=&self.stack_layers[temp];
        let scratch=self.title_post_scratch.as_ref().unwrap();
        let params=StyleParams {width:w,height:h,frame_width:w,op:31,x0:0,y0:0,offset_x:0,offset_y:0,color:[0.0;4],matrix:to_authored};
        self.write_params(&params);
        self.stack_pass(&self.title_style_pipeline,source,blurred,count.div_ceil(WORKGROUP_SIZE),"title premultiply authored");
        let r=(radius/1.7).round().max(1.0).min(w.max(h) as f32) as i32;
        for _ in 0..3 {
            self.write_params(&StyleParams {op:32,offset_x:r,offset_y:0,..params});
            self.stack_pass(&self.title_style_pipeline,blurred,scratch,h.div_ceil(WORKGROUP_SIZE),"title focus horizontal");
            self.write_params(&StyleParams {op:32,offset_x:r,offset_y:1,..params});
            self.stack_pass(&self.title_style_pipeline,scratch,blurred,w.div_ceil(WORKGROUP_SIZE),"title focus vertical");
        }
        self.write_params(&StyleParams {op:33,color:[if vertical {1.0}else{0.0},band,0.0,0.0],..params});
        self.stack_pass(&self.title_style_pipeline,blurred,source,count.div_ceil(WORKGROUP_SIZE),"title focus authored mix");
        self.write_params(&StyleParams {op:11,matrix:self.authored_to_working,..params});
        self.stack_pass(&self.title_style_pipeline,source,scratch,count.div_ceil(WORKGROUP_SIZE),"title focus working space");
        let mut enc=self.encoder(Some("title focus copy"));
        enc.copy_buffer_to_buffer(scratch,0,source,0,u64::from(count)*16);
        self.submit(enc);
        true
    }

    /// Whole-layer blur motion. The radius uses clipped painted bounds, reduced
    /// on GPU, and three premultiplied box passes match the byte title painter.
    pub fn stack_title_motion_blur(&mut self, slot: usize, away: f32) -> bool {
        self.stack_title_motion_blur_translate(slot,away,[0.0;4],[0.0;4],false)
    }

    /// Blur before whole-pixel translation. Travel uses the original painted
    /// height; sampling includes the blur halo, not just the original bounds.
    /// `clip_rows` retains only destination rows in the original painted box.
    pub fn stack_title_motion_blur_translate(&mut self, slot: usize, away: f32, enter: [f32;4], exit: [f32;4], clip_rows: bool) -> bool {
        self.stack_title_motion_blur_pose(slot,[away,0.12,0.0,0.0],enter,exit,None,false,clip_rows)
    }

    /// Scale the blurred byte sprite around the original painted bounds.
    /// Motion alpha is applied during sampling, as in the CPU pose operation.
    /// With `solid`, choose the deepest covered cell instead of the point.
    pub fn stack_title_motion_blur_scale(&mut self, slot: usize, away: f32, scale: f32, opacity: f32, anchor: [f32;2], solid: bool) -> bool {
        if !scale.is_finite() || scale<=0.0 || !opacity.is_finite() || anchor.iter().any(|v|!v.is_finite()) {return false;}
        self.stack_title_motion_blur_pose(slot,[away,0.12,0.0,0.0],[0.0;4],[0.0;4],Some([scale,opacity.clamp(0.0,1.0),anchor[0].clamp(0.0,1.0),anchor[1].clamp(0.0,1.0)]),solid,false)
    }

    /// Blur amounts are entrance/exit (remaining progress, radius per painted height).
    /// Retains translation, scaling and row clipping in the same sampling pass.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_title_motion_blur_pose(&mut self, slot: usize, blur: [f32;4], enter: [f32;4], exit: [f32;4], scaled: Option<[f32;4]>, solid: bool, clip_rows: bool) -> bool {
        if blur.iter().any(|v| !v.is_finite() || *v<0.0)
            || blur[0]>1.0 || blur[2]>1.0
            || enter.iter().chain(exit.iter()).any(|v|!v.is_finite()) {return false;}
        let scaled=match scaled {
            Some([scale,opacity,x,y]) => {
                if !scale.is_finite() || scale<=0.0 || !opacity.is_finite() || !x.is_finite() || !y.is_finite() {return false;}
                Some([scale,opacity.clamp(0.0,1.0),x.clamp(0.0,1.0),y.clamp(0.0,1.0)])
            }
            None=>None,
        };
        let translates=enter[2..].iter().chain(exit[2..].iter()).any(|v|*v!=0.0);
        if blur[0]*blur[1]==0.0 && blur[2]*blur[3]==0.0 {
            if let Some([scale,opacity,x,y])=scaled {return self.stack_title_motion_scale(slot,enter,exit,scale,opacity,[x,y],solid,clip_rows);}
            return (!translates && !clip_rows) || self.stack_title_motion_translate(slot,enter,exit,clip_rows);
        }
        let Some(temp)=self.stack_title_object_slot(2) else {return false;};
        if temp==slot || !self.stack_title_alpha(slot,[0.0;4],1.0) {return false;}
        let Some(to_authored)=inverse(self.authored_to_working) else {return false;};
        let (w,h)=self.stack_dims;let bytes=u64::from(w)*u64::from(h)*16;
        let size=bytes+16+if solid {8+u64::from(w.min(319))*u64::from(h.min(319))*4}else{0};
        if size>u64::from(self.device.limits().max_storage_buffer_binding_size) {return false;}
        let buffer=||self.device.create_buffer(&wgpu::BufferDescriptor {
            label:Some("title blur image and bounds"),size,
            usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_SRC|wgpu::BufferUsages::COPY_DST,
            mapped_at_creation:false,
        });
        if self.title_post_scratch.as_ref().is_none_or(|b|b.size()<size) {self.title_post_scratch=Some(buffer());}
        if self.stack_layers[temp].size()<size {self.stack_layers[temp]=std::sync::Arc::new(buffer());}
        if !self.stack_title_alpha_bounds(slot) {return false;}
        if solid {self.ensure_title_solid_grid();}
        let source=&self.stack_layers[slot];let blurred=&self.stack_layers[temp];
        let scratch=self.title_post_scratch.as_ref().unwrap();
        let mut enc=self.encoder(Some("title blur bounds"));
        enc.copy_buffer_to_buffer(scratch,0,blurred,0,16);self.submit(enc);
        let params=StyleParams {width:w,height:h,frame_width:w,op:44,x0:0,y0:0,
            offset_x:0,offset_y:0,color:blur,matrix:to_authored};
        self.write_params(&params);
        self.stack_pass(&self.title_style_pipeline,source,blurred,(w*h).div_ceil(WORKGROUP_SIZE),"title motion blur premultiply");
        for _ in 0..3 {
            self.write_params(&StyleParams {op:45,offset_y:0,..params});
            self.stack_pass(&self.title_style_pipeline,blurred,scratch,h.div_ceil(WORKGROUP_SIZE),"title motion blur horizontal");
            self.write_params(&StyleParams {op:45,offset_y:1,..params});
            self.stack_pass(&self.title_style_pipeline,scratch,blurred,w.div_ceil(WORKGROUP_SIZE),"title motion blur vertical");
        }
        if solid {self.stack_title_solid_anchor(blurred,true);}
        self.write_params(&StyleParams {offset_x:i32::from(solid),offset_y:i32::from(clip_rows),op:if scaled.is_some() {48}else if translates {47}else{46},matrix:[enter,exit,scaled.unwrap_or([0.0;4])],..params});
        self.stack_pass(&self.title_style_pipeline,blurred,source,(w*h).div_ceil(WORKGROUP_SIZE),"title motion blur bytes");
        self.write_params(&StyleParams {op:11,matrix:self.authored_to_working,..params});
        self.stack_pass(&self.title_style_pipeline,source,scratch,(w*h).div_ceil(WORKGROUP_SIZE),"title motion blur working space");
        let mut enc=self.encoder(Some("title motion blur copy"));
        enc.copy_buffer_to_buffer(scratch,0,source,0,bytes);self.submit(enc);true
    }

    /// Analytic fitted rectangle coverage, using the authored title painter's pixel tests.
    pub fn stack_title_rectangle(&mut self, slot: usize, bounds: [f32;4], pivot: [f32;2], radians: f32, radius: f32, fill: [u8;4], stroke: Option<([u8;4],f32)>, straight: bool, gradient: Option<&TitleFill>) -> bool {
        if !bounds.iter().chain(pivot.iter()).chain([radians,radius].iter()).all(|v|v.is_finite()) || !self.stack_title_alpha(slot,[0.0;4],1.0) {return false;}
        let colour=|c:[u8;4]| {
            let rgb:[f32;3]=std::array::from_fn(|i| {
                let v=if straight {c[i] as f32}else{c[i] as f32*c[3] as f32/255.0};
                let x=v/255.0;if x<=0.04045 {x/12.92}else{((x+0.055)/1.055).powf(2.4)}
            });
            let rgb=self.authored_to_working.map(|row|row[0]*rgb[0]+row[1]*rgb[1]+row[2]*rgb[2]);
            [rgb[0],rgb[1],rgb[2],c[3] as f32/255.0]
        };
        let mut stops=[[0u32;4];8];stops[0]=colour(stroke.map_or([0;4],|s|s.0)).map(f32::to_bits);
        let (w,h)=self.stack_dims;
        let mut params=StyleParams{width:w,height:h,frame_width:w,op:34,x0:0,y0:0,offset_x:0,offset_y:0,color:colour(fill),
            matrix:[bounds,[pivot[0],pivot[1],radians.cos(),radians.sin()],[radius,stroke.map_or(0.0,|s|s.1),0.0,0.0]]};
        if let Some(gradient)=gradient {
            let TitleFill::Gradient{geometry,stops:gradient_stops}=gradient else {return false;};
            if geometry.iter().any(|v|!v.is_finite()) || gradient_stops.is_empty() || gradient_stops.len()>16 || gradient_stops.iter().any(|s|!s.0.is_finite()) {return false;}
            params.op=37;params.color = *geometry;params.offset_x=gradient_stops.len() as i32;params.offset_y=i32::from(straight);
            params.frame_width=u32::from_le_bytes(stroke.map_or([0;4],|s|s.0));
            stops=[[0;4];8];let mut sorted=gradient_stops.clone();sorted.sort_by(|a,b|a.0.total_cmp(&b.0));
            for (i,stop) in sorted.iter().enumerate() {stops[i/2][i%2*2]=stop.0.to_bits();stops[i/2][i%2*2+1]=u32::from_le_bytes(stop.1);}
        }
        self.write_params(&FillParams{base:params,stops});
        let layer=&self.stack_layers[slot];let scratch=self.title_post_scratch.as_ref().unwrap();
        self.stack_pass(&self.title_style_pipeline,layer,scratch,(w*h).div_ceil(WORKGROUP_SIZE),"fitted title rectangle");
        if gradient.is_some() {
            self.write_params(&StyleParams{op:11,width:w,height:h,frame_width:w,matrix:self.authored_to_working,..params});
            self.stack_pass(&self.title_style_pipeline,scratch,layer,(w*h).div_ceil(WORKGROUP_SIZE),"rectangle gradient working space");
            return true;
        }
        let mut enc=self.encoder(Some("title rectangle copy"));enc.copy_buffer_to_buffer(scratch,0,layer,0,u64::from(w)*u64::from(h)*16);self.submit(enc);true
    }

    /// A vector path's painted layer drawn into `slot` on the GPU, as the
    /// CPU's `paint_filled` draws it (#2201): `path` holds its flattened
    /// geometry, offset to its padded box as the CPU offsets it. Its glow is
    /// `blur_alpha`'s: three rounds of box passes over the stroke's byte
    /// mask within the box, laid under the fill and stroke.
    pub fn stack_title_path(&mut self, slot: usize, path: &TitlePath) -> bool {
        let [x0,y0,x1,y1]=path.bounds;
        let (w,h)=self.stack_dims;
        if x1<=x0 || y1<=y0 || x1>w || y1>h
            || !path.fill_edges.iter().chain(path.stroke_segments.iter()).flatten().chain(path.fill.iter()).chain(path.stroke.iter()).chain([path.half].iter()).all(|v|v.is_finite())
            || !self.stack_title_alpha(slot,[0.0;4],1.0) {return false;}
        let mut words:Vec<u32>=vec![path.fill_edges.len() as u32,path.stroke_segments.len() as u32,x0,y0,x1,y1,path.half.to_bits(),0];
        words.extend(path.fill.iter().chain(path.stroke.iter()).map(|v|v.to_bits()));
        let glow=path.glow.filter(|(radius,strength)|radius.round()>=1.0 && *strength>0.0 && radius.is_finite() && strength.is_finite());
        // Each sample row's segments, a row either side to spare: the pass
        // still tests each one exactly, but only near its own rows.
        let rows=2*(y1-y0) as usize;
        let span=|lo:f32,hi:f32|-> (usize,usize) {
            let first=((lo*2.0-0.5).ceil()-1.0).max(0.0);let last=((hi*2.0-0.5).ceil()+1.0).max(0.0);
            ((first as usize).min(rows),(last as usize).min(rows))
        };
        let bins=|segments:&[[f32;4]],r:f32| {
            let mut counts=vec![0u32;rows+1];
            let spans:Vec<(usize,usize)>=segments.iter().map(|s|span(s[1].min(s[3])-r,s[1].max(s[3])+r)).collect();
            for &(a,b) in &spans {for c in &mut counts[a..b] {*c+=1;}}
            let mut offsets=vec![0u32;rows+1];
            for i in 0..rows {offsets[i+1]=offsets[i]+counts[i];}
            let mut index=vec![0u32;offsets[rows] as usize];let mut fill=offsets.clone();
            for (e,&(a,b)) in spans.iter().enumerate() {for row in a..b {index[fill[row] as usize]=e as u32;fill[row]+=1;}}
            (offsets,index)
        };
        let (fill_offsets,fill_index)=bins(&path.fill_edges,0.0);
        let (stroke_offsets,stroke_index)=bins(&path.stroke_segments,path.half);
        let segments=32+4*(path.fill_edges.len()+path.stroke_segments.len());
        let fo=segments;let so=fo+rows+1;let fi=so+rows+1;let si=fi+fill_index.len();
        words.extend([glow.map_or(0.0,|g|g.1).to_bits(),rows as u32,fo as u32,so as u32]);
        let mut stops=[[0u32;4];8];let mut stop_count=0;
        let mut gradient_words=[0u32;12];
        if let Some((fill,unmap))=&path.gradient {
            let TitleFill::Gradient{geometry,stops:gradient_stops}=fill else {return false;};
            if geometry.iter().chain(unmap.iter()).any(|v|!v.is_finite()) || gradient_stops.is_empty() || gradient_stops.len()>16 || gradient_stops.iter().any(|s|!s.0.is_finite()) {return false;}
            let mut sorted=gradient_stops.clone();sorted.sort_by(|a,b|a.0.total_cmp(&b.0));
            for (i,stop) in sorted.iter().enumerate() {stops[i/2][i%2*2]=stop.0.to_bits();stops[i/2][i%2*2+1]=u32::from_le_bytes(stop.1);}
            stop_count=sorted.len() as i32;
            for (w,v) in gradient_words.iter_mut().zip(geometry.iter().chain(unmap.iter())) {*w=v.to_bits();}
        }
        words.extend(gradient_words);
        words.extend(path.fill_edges.iter().chain(path.stroke_segments.iter()).flatten().map(|v|v.to_bits()));
        words.extend(fill_offsets.iter().map(|o|o+fi as u32));
        words.extend(stroke_offsets.iter().map(|o|o+si as u32));
        words.extend(fill_index);
        words.extend(stroke_index);
        let geometry=self.device.create_buffer(&wgpu::BufferDescriptor{label:Some("title path geometry"),size:(words.len()*4) as u64,usage:wgpu::BufferUsages::STORAGE,mapped_at_creation:true});
        geometry.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(&words));
        geometry.unmap();
        let layer=&self.stack_layers[slot];let scratch=self.title_post_scratch.as_ref().unwrap();
        let groups=(w*h).div_ceil(WORKGROUP_SIZE);
        if let Some((radius,_))=glow {
            // The stroke's mask into the scratch, then each box pass between
            // the scratch and the slot, ending in the scratch.
            self.write_params(&StyleParams{width:w,height:h,frame_width:w,op:52,x0:0,y0:0,offset_x:0,offset_y:0,color:[0.0;4],matrix:self.authored_to_working});
            self.stack_pass(&self.title_style_pipeline,&geometry,scratch,groups,"title path glow mask");
            for _ in 0..3 {
                for (axis,(from,to)) in [(scratch,layer.as_ref()),(layer.as_ref(),scratch)].into_iter().enumerate() {
                    self.write_params(&StyleParams{width:w,height:h,frame_width:w,op:53,x0,y0,offset_x:radius.round() as i32,offset_y:axis as i32,color:[x1 as f32,y1 as f32,0.0,0.0],matrix:self.authored_to_working});
                    self.stack_pass(&self.title_style_pipeline,from,to,groups,"title path glow blur");
                }
            }
        }
        self.write_params(&FillParams{base:StyleParams{width:w,height:h,frame_width:w,op:51,x0:0,y0:0,offset_x:stop_count,offset_y:0,color:[0.0;4],matrix:self.authored_to_working},stops});
        self.stack_pass(&self.title_style_pipeline,&geometry,scratch,groups,"title vector path");
        let mut enc=self.encoder(Some("title path copy"));enc.copy_buffer_to_buffer(scratch,0,layer,0,u64::from(w)*u64::from(h)*16);self.submit(enc);true
    }

    /// Whether a picture of `texels` fits one GPU storage binding.
    pub fn stack_title_image_fits(&self, texels: usize) -> bool {
        (texels as u64).saturating_mul(4) <= u64::from(self.device.limits().max_storage_buffer_binding_size)
    }

    /// Whether the picture `id` is on the GPU already.
    pub fn stack_title_image_resident(&self, id: u64) -> bool {
        self.stack_sprites.iter().any(|r| r.id == id)
    }

    /// Layer `slot` takes an image layer's picture fitted into its box
    /// (#2342): `texels` the decoded picture, packed RGBA8, `size` wide
    /// and high, kept on the GPU as `id`; `rect` the drawn rectangle's
    /// left and top and the picture's pixels per frame pixel across and
    /// down, and `clip` the covered pixels (x0, y0,
    /// x1, y1), as `klipp_title::image_placement` gives them. Sampled as
    /// the CPU's layer is, so a keyed box is one pass and no upload.
    pub fn stack_title_image(&mut self, slot: usize, id: u64, texels: &[u32], size: [u32; 2], rect: [f32; 4], clip: [u32; 4]) -> bool {
        if self.is_device_lost() || slot >= self.stack_layers.len() || size[0] == 0 || size[1] == 0
            || texels.len() as u64 != u64::from(size[0]) * u64::from(size[1]) || !self.stack_title_image_fits(texels.len())
        { return false; }
        let (w, h) = self.stack_dims;
        let count = u64::from(w) * u64::from(h);
        if self.title_post_scratch.as_ref().is_none_or(|b| b.size() < count * 16) {
            self.title_post_scratch = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title post scratch"),
                size: count * 16,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let pos = self.resident_sprite(id, texels);
        let picture = &self.stack_sprites[pos].buffer;
        let layer = &self.stack_layers[slot];
        let scratch = self.title_post_scratch.as_ref().unwrap();
        let groups = (w * h).div_ceil(WORKGROUP_SIZE);
        let params = StyleParams { width: w, height: h, frame_width: w, op: 54, x0: 0, y0: 0, offset_x: size[0] as i32, offset_y: size[1] as i32, color: rect,
            matrix: [clip.map(|v| v as f32), [0.0; 4], [0.0; 4]] };
        self.write_params(&params);
        self.stack_pass(&self.title_style_pipeline, picture, scratch, groups, "title image fit");
        self.write_params(&StyleParams { op: 11, matrix: self.authored_to_working, ..params });
        self.stack_pass(&self.title_style_pipeline, scratch, layer, groups, "title image working");
        true
    }

    /// Match byte layers or the completed scene's packed-half source boundary.
    pub fn stack_title_precision(&mut self,slot:usize,bytes:bool)->bool {
        if !self.stack_title_alpha(slot,[0.0;4],1.0) {return false;}
        let Some(to_authored)=inverse(self.authored_to_working) else {return false;};
        let (w,h)=self.stack_dims;let count=w*h;
        let layer=&self.stack_layers[slot];let scratch=self.title_post_scratch.as_ref().unwrap();
        let params=StyleParams{width:w,height:h,frame_width:w,op:35,x0:0,y0:0,offset_x:i32::from(bytes),offset_y:0,color:[0.0;4],matrix:to_authored};
        self.write_params(&params);self.stack_pass(&self.title_style_pipeline,layer,scratch,count.div_ceil(WORKGROUP_SIZE),"title scene quantization");
        self.write_params(&StyleParams{op:11,matrix:self.authored_to_working,..params});self.stack_pass(&self.title_style_pipeline,scratch,layer,count.div_ceil(WORKGROUP_SIZE),"title scene quantized working");true
    }

    /// Match the CPU title raster's bounding box (alpha >= half a byte).
    /// Reduction stays on GPU; sub-threshold pixels inside the box survive.
    pub fn stack_title_crop_to_alpha_bounds(&mut self, slot: usize) -> bool {
        if !self.stack_title_alpha_bounds(slot) {return false;}
        let (w,h)=self.stack_dims;
        self.write_params(&StyleParams {width:w,height:h,frame_width:w,op:40,x0:0,y0:0,
            offset_x:0,offset_y:0,color:[0.0;4],matrix:[[0.0;4];3]});
        self.stack_pass(&self.title_style_pipeline,self.title_post_scratch.as_ref().unwrap(),
            &self.stack_layers[slot],(w*h).div_ceil(WORKGROUP_SIZE),"title scene crop");
        true
    }

    /// Move a painted byte layer by the CPU shape motion's whole-pixel offset.
    /// Each motion is [away, distance, direction_x, direction_y]; travel uses
    /// the painted layer's clipped height, measured entirely on GPU.
    /// `clip_rows` retains only destination rows in that original box.
    pub fn stack_title_motion_translate(&mut self, slot: usize, enter: [f32;4], exit: [f32;4], clip_rows: bool) -> bool {
        self.stack_title_motion_pose(slot,enter,exit,None,false,clip_rows)
    }

    /// Scale around a point in the clipped source bounds, sampling the CPU's
    /// half-float sprite in premultiplied authored colour before byte rounding.
    /// With `solid`, choose the deepest covered cell instead of the point.
    /// `clip_rows` applies the original row window after scaling/translation.
    pub fn stack_title_motion_scale(&mut self, slot: usize, enter: [f32;4], exit: [f32;4], scale: f32, opacity: f32, anchor: [f32;2], solid: bool, clip_rows: bool) -> bool {
        if !scale.is_finite() || scale<=0.0 || !opacity.is_finite() || anchor.iter().any(|v|!v.is_finite()) {return false;}
        self.stack_title_motion_pose(slot,enter,exit,Some([scale,opacity.clamp(0.0,1.0),anchor[0].clamp(0.0,1.0),anchor[1].clamp(0.0,1.0)]),solid,clip_rows)
    }

    fn stack_title_motion_pose(&mut self, slot: usize, enter: [f32;4], exit: [f32;4], scaled: Option<[f32;4]>, solid: bool, clip_rows: bool) -> bool {
        if enter.iter().chain(exit.iter()).any(|v|!v.is_finite())
            || !self.stack_title_alpha(slot,[0.0;4],1.0) {return false;}
        let (w,h)=self.stack_dims;
        let bytes=u64::from(w)*u64::from(h)*16;
        let size=bytes+16+if solid {8+u64::from(w.min(319))*u64::from(h.min(319))*4}else{0};
        if size>u64::from(self.device.limits().max_storage_buffer_binding_size) {return false;}
        // Four bounds words precede the source image. Keeping both in one
        // read buffer avoids in-place translation races or a CPU bounds read.
        if self.title_post_scratch.as_ref().is_none_or(|b|b.size()<size) {
            self.title_post_scratch=Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label:Some("title motion source and bounds"),size,
                usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_SRC|wgpu::BufferUsages::COPY_DST,
                mapped_at_creation:false,
            }));
        }
        if scaled.is_some() {
            let Some(to_authored)=inverse(self.authored_to_working) else {return false;};
            let params=StyleParams {width:w,height:h,frame_width:w,op:42,x0:0,y0:0,
                offset_x:0,offset_y:0,color:[0.0;4],matrix:to_authored};
            self.write_params(&params);
            let source=&self.stack_layers[slot];let scratch=self.title_post_scratch.as_ref().unwrap();
            self.stack_pass(&self.title_style_pipeline,source,scratch,(w*h).div_ceil(WORKGROUP_SIZE),"title half-float scale source");
            let mut enc=self.encoder(Some("title scale source copy"));
            enc.copy_buffer_to_buffer(scratch,0,source,0,bytes);self.submit(enc);
        }
        if !self.stack_title_alpha_bounds(slot) {return false;}
        if solid {self.ensure_title_solid_grid();}
        let source=&self.stack_layers[slot];
        let scratch=self.title_post_scratch.as_ref().unwrap();
        let mut enc=self.encoder(Some("title motion source copy"));
        enc.copy_buffer_to_buffer(source,0,scratch,16,bytes);self.submit(enc);
        if solid {self.stack_title_solid_anchor(scratch,false);}
        self.write_params(&StyleParams {width:w,height:h,frame_width:w,op:if scaled.is_some() {43}else{41},x0:0,y0:0,
            offset_x:i32::from(solid),offset_y:i32::from(clip_rows),color:[0.0;4],matrix:[enter,exit,scaled.unwrap_or([0.0;4])]});
        self.stack_pass(&self.title_style_pipeline,scratch,source,(w*h).div_ceil(WORKGROUP_SIZE),"title whole-layer motion");
        if scaled.is_some() {
            self.write_params(&StyleParams {width:w,height:h,frame_width:w,op:11,x0:0,y0:0,
                offset_x:0,offset_y:0,color:[0.0;4],matrix:self.authored_to_working});
            self.stack_pass(&self.title_style_pipeline,source,scratch,(w*h).div_ceil(WORKGROUP_SIZE),"title scaled working space");
            let mut enc=self.encoder(Some("title scaled layer copy"));
            enc.copy_buffer_to_buffer(scratch,0,source,0,bytes);self.submit(enc);
        }
        true
    }

    // A floor-sized cell yields at most 319 cells along either axis.
    // Reuse the bounds scratch for coverage; the distance transform lives
    // after the prefixed image, leaving source pixels and bounds untouched.
    fn ensure_title_solid_grid(&mut self) {
        let (w,h)=self.stack_dims;
        let size=(8+u64::from(w.min(319))*u64::from(h.min(319)))*4;
        if self.title_scene_bounds.as_ref().is_none_or(|b|b.size()<size) {
            self.title_scene_bounds=Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label:Some("title solid anchor grid"),size,
                usage:wgpu::BufferUsages::STORAGE,mapped_at_creation:false,
            }));
        }
    }

    fn stack_title_solid_anchor(&self, source: &wgpu::Buffer, blurred: bool) {
        let (w,h)=self.stack_dims;
        let grid=self.title_scene_bounds.as_ref().unwrap();
        let params=StyleParams {width:w,height:h,frame_width:w,op:49,x0:0,y0:0,
            offset_x:i32::from(blurred),offset_y:0,color:[0.0;4],matrix:[[0.0;4];3]};
        self.write_params(&params);
        self.stack_pass(&self.title_style_pipeline,source,grid,(w.min(319)*h.min(319)).div_ceil(WORKGROUP_SIZE),"title solid anchor coverage");
        self.write_params(&StyleParams {op:50,..params});
        self.stack_pass(&self.title_style_pipeline,grid,source,1,"title solid anchor distance");
    }

    /// Leave [x0,y0,x1,y1] in the first four scratch words.
    fn stack_title_alpha_bounds(&mut self, slot: usize) -> bool {
        if !self.stack_title_alpha(slot, [0.0; 4], 1.0) { return false; }
        let (w,h)=self.stack_dims;
        let bytes=u64::from(h)*8;
        if self.title_scene_bounds.as_ref().is_none_or(|b|b.size()<bytes) {
            self.title_scene_bounds=Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label:Some("title scene row bounds"),size:bytes,
                usage:wgpu::BufferUsages::STORAGE,mapped_at_creation:false,
            }));
        }
        let layer=&self.stack_layers[slot];
        let rows=self.title_scene_bounds.as_ref().unwrap();
        let scratch=self.title_post_scratch.as_ref().unwrap();
        let params=StyleParams {width:w,height:h,frame_width:w,op:38,x0:0,y0:0,
            offset_x:0,offset_y:0,color:[0.0;4],matrix:[[0.0;4];3]};
        self.write_params(&params);
        self.stack_pass(&self.title_style_pipeline,layer,rows,h.div_ceil(WORKGROUP_SIZE),"title scene row bounds");
        self.write_params(&StyleParams {op:39,..params});
        self.stack_pass(&self.title_style_pipeline,rows,scratch,1,"title scene bounds reduction");
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
        self.title_over(source,destination,opacity,mode,false)
    }

    /// An undressed normal shape paints premultiplied encoded colour directly.
    pub fn stack_title_painted_over(&mut self,source:usize,destination:usize)->bool {
        self.title_over(source,destination,1.0,crate::BlendMode::Normal,true)
    }

    fn title_over(&mut self,source:usize,destination:usize,opacity:f32,mode:crate::BlendMode,painted:bool)->bool {
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
            op: if painted {36} else {10},
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
            fill,
            rich_slot,
            face_slot,
            colour_slot,
            bounds,
        } = face;
        if rich_slot.is_some_and(|i| i >= self.stack_layers.len() || i == slot)
            || face_slot.is_some_and(|i| i >= self.stack_layers.len() || i == slot || Some(i) == rich_slot)
            || colour_slot.is_some_and(|i| i >= self.stack_layers.len() || i == slot || Some(i) == rich_slot || Some(i) == face_slot)
        {
            return false;
        }
        if !fill_alpha.is_finite() || (fill.is_some() && bounds.iter().any(|v| !v.is_finite())) {
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
        let face_layer = &self.stack_layers[face_slot.unwrap_or(slot)];
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
                    if let Some(TitleFill::Metal(metal)) = &fill {
                        if !self.title_metal_face(s, base, metal, bounds, fill_alpha) {
                            return false;
                        }
                    } else if let Some(fill) = &fill {
                        let mut params = FillParams {
                            base: StyleParams {
                                op: 22,
                                offset_x: write_cutoff,
                                matrix: [bounds, [0.0; 4], [0.0; 4]],
                                ..base
                            },
                            stops: [[0; 4]; 8],
                        };
                        match fill {
                            TitleFill::Pair { top, bottom } => {
                                params.base.offset_y = 0;
                                params.base.color = top.map(|v| v as f32 / 255.0);
                                params.base.matrix[2] = bottom.map(|v| v as f32 / 255.0);
                            }
                            TitleFill::Gradient { geometry, stops } => {
                                if geometry.iter().any(|v| !v.is_finite())
                                    || stops.is_empty()
                                    || stops.len() > 16
                                    || stops.iter().any(|s| !s.0.is_finite())
                                {
                                    return false;
                                }
                                params.base.offset_y = 1;
                                params.base.color[0] = stops.len() as f32;
                                params.base.matrix[1] = *geometry;
                                let mut sorted = stops.clone();
                                sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
                                for (i, stop) in sorted.iter().enumerate() {
                                    params.stops[i / 2][(i % 2) * 2] = stop.0.to_bits();
                                    params.stops[i / 2][(i % 2) * 2 + 1] =
                                        u32::from_le_bytes(stop.1);
                                }
                            }
                            TitleFill::Metal { .. } => unreachable!("drawn above"),
                            TitleFill::Pattern {
                                colors,
                                mode,
                                spacing,
                            } => {
                                if !spacing.is_finite() || *mode > 3 {
                                    return false;
                                }
                                params.base.offset_y = 2;
                                params.base.color = colors[0].map(|v| v as f32 / 255.0);
                                params.base.matrix[1] = [*mode as f32, spacing.max(1.0), 0.0, 0.0];
                                params.base.matrix[2] = colors[1].map(|v| v as f32 / 255.0);
                            }
                        }
                        self.write_params(&params);
                        self.stack_pass(
                            &self.title_style_pipeline,
                            face_layer,
                            &s.composite,
                            (count as u32).div_ceil(WORKGROUP_SIZE),
                            "title procedural fill",
                        );
                    } else if let Some(rich) = rich_slot {
                        pass(
                            30,
                            &self.stack_layers[rich],
                            &s.composite,
                            [0, 0],
                            [0.0; 4],
                            to_authored,
                        );
                    } else {
                        pass(
                            6,
                            face_layer,
                            &s.composite,
                            [write_cutoff, 0],
                            [0.0, 0.0, 0.0, fill_alpha],
                            to_authored,
                        );
                    }
                    // Colour glyphs in their own colours, over the face and
                    // under the inner shadows, as the CPU paints them.
                    if let Some(colour) = colour_slot {
                        pass(
                            6,
                            &self.stack_layers[colour],
                            &s.composite,
                            [i32::MAX, 0],
                            [0.0, 0.0, 0.0, 1.0],
                            to_authored,
                        );
                    }
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
    /// A metal face over the styles behind it (#2706): the face's
    /// coverage blurred by the bevel into a height, packed with the
    /// coverage, and shaded from that height's slope.
    fn title_metal_face(&self, s: &StyleScratch, base: StyleParams, m: &TitleMetal, bounds: [f32; 4], alpha: f32) -> bool {
        let fields = [m.roughness, m.edge, m.highlights, m.glints, m.anisotropy, m.anisotropy_radians, m.bevel_px, m.depth, m.key.1, m.rim.1, m.studio, m.sky, m.seed, m.glint_cell];
        if fields.iter().chain(&m.tint).chain(&m.key.0).chain(&m.key.2).chain(&m.rim.0).chain(&m.rim.2).chain(&m.sky_tint).any(|v| !v.is_finite()) || m.bevel_px < 0.0 || m.bevel_px > 32767.0 {
            return false;
        }
        let (w, h) = (base.width, base.height);
        let count = u64::from(w) * u64::from(h);
        let step = |op: u32, src: &wgpu::Buffer, dst: &wgpu::Buffer| {
            self.write_params(&StyleParams { op, ..base });
            self.stack_pass(&self.title_style_pipeline, src, dst, (count as u32).div_ceil(WORKGROUP_SIZE), "title metal height");
        };
        step(60, &s.mask, &s.work);
        if !self.record_alpha_blur(&s.work, &s.work, &s.blur, w, h, m.bevel_px.max(1.0)) {
            return false;
        }
        step(61, &s.mask, &s.work);
        // A box blurred three times rises over about 2.5 radii: the
        // slope that turns its steepest point by `depth` x 60 degrees.
        let slope = m.depth.clamp(0.0, 1.0) * 1.73 * 2.5 * m.bevel_px.max(1.0);
        let f = |v: f32| v.to_bits();
        let stop = |a: [f32; 4]| a.map(f);
        let (fw, fh) = self.stack_dims;
        let centre = [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5];
        let mut stops = [[0u32; 4]; 8];
        stops[0] = stop([m.rim.0[0], m.rim.0[1], m.rim.0[2], m.rim.1]);
        stops[1] = stop([m.rim.2[0], m.rim.2[1], m.rim.2[2], m.anisotropy]);
        stops[2] = stop([m.studio, m.sky, m.anisotropy_radians, (fw.max(fh) as f32) * 1.2]);
        stops[3] = stop([m.sky_tint[0], m.sky_tint[1], m.sky_tint[2], m.seed]);
        stops[4] = stop([centre[0], centre[1], m.glint_cell.max(8.0), 0.0]);
        stops[5] = stop([bounds[0], bounds[1], 0.0, 0.0]);
        self.write_params(&FillParams {
            base: StyleParams {
                op: 62,
                color: [m.tint[0], m.tint[1], m.tint[2], alpha],
                matrix: [
                    [m.roughness, m.edge, slope, m.highlights],
                    [m.key.0[0], m.key.0[1], m.key.0[2], m.key.1],
                    [m.key.2[0], m.key.2[1], m.key.2[2], m.glints],
                ],
                ..base
            },
            stops,
        });
        self.stack_pass(&self.title_style_pipeline, &s.work, &s.composite, (count as u32).div_ceil(WORKGROUP_SIZE), "title metal face");
        true
    }

    /// Resident finishing scratch only; excludes shared stack and source caches.
    pub fn title_style_scratch_bytes(&self) -> u64 {
        self.title_style_scratch
            .as_ref()
            .map_or(0, |s| s.count * 28)
    }
}
