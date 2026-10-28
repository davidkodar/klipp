//! Generators and grain on the GPU (#2890): the living gradient, grain and
//! segments layers, the solid, noise and particle generators, and grain and
//! film grain over a picture, with their params and shaders.

use super::*;

impl GpuEffectsPipeline {
    /// Apply film grain overlay on GPU.
    pub fn apply_grain_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        amount: f32,
        size: f32,
        shadows: f32,
        highlights: f32,
        monochrome: bool,
        frame_seed: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuGrainParams {
            width,
            height,
            amount,
            size,
            shadows,
            highlights,
            frame_seed,
            monochrome: if monochrome { 1 } else { 0 },
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.dispatch(&self.grain_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Dispatch-only film grain overlay pass.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_grain_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        amount: f32,
        size: f32,
        shadows: f32,
        highlights: f32,
        monochrome: bool,
        frame_seed: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuGrainParams {
            width, height, amount, size, shadows, highlights, frame_seed,
            monochrome: if monochrome { 1 } else { 0 },
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.grain_pipeline, bind_group, workgroups);
    }

    /// Draw the native living gradient directly in a stack layer. Only
    /// parameters cross from the CPU; pixels remain on the GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_living_gradient(&mut self, i: usize, a: [u8; 4], b: [u8; 4], angle_deg: f32, drift: f32, glow: f32, grain: f32, secs: f64) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() { return false; }
        let (width, height) = self.stack_dims;
        let t = secs as f32;
        let theta = (angle_deg + drift * t).to_radians();
        self.write_params(&LivingGradientParams {
            width, height, glow, grain,
            a: a.map(f32::from), b: b.map(f32::from),
            dir: [theta.sin(), theta.cos()],
            centre: [width as f32 * (0.5 + 0.28 * (t * 0.33).cos()), height as f32 * (0.5 + 0.22 * (t * 0.21).sin())],
            to_working: self.authored_to_working,
        });
        self.stack_pass(&self.living_gradient_pipeline, self.stack_acc.as_ref().unwrap(), &self.stack_layers[i], (width * height).div_ceil(WORKGROUP_SIZE), "stack_living_gradient");
        true
    }

    /// Layer `i` as the native Grain generator draws it (#2067): `color`
    /// with monochrome noise `amount` deep in cells `size_px` wide at
    /// 1080p, a new grain `speed` times a second. The CPU's
    /// `render_grain` is its reference: the same hash, cell and depth.
    pub fn stack_layer_grain(&mut self, i: usize, color: [u8; 4], amount: f32, size_px: f32, speed: f32, secs: f64) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() { return false; }
        let (width, height) = self.stack_dims;
        let scale = (height as f32 / 1080.0).max(0.1);
        self.write_params(&GrainGeneratorParams {
            width, height,
            frame: (secs as f32 * speed.max(0.0)).floor() as i64 as u32,
            _pad: 0,
            color: color.map(f32::from),
            cell: (size_px * scale).max(1.0),
            depth: amount.clamp(0.0, 1.0) * 110.0,
            _p: [0.0; 2],
            to_working: self.authored_to_working,
        });
        self.stack_pass(&self.grain_generator_pipeline, self.stack_acc.as_ref().unwrap(), &self.stack_layers[i], (width * height).div_ceil(WORKGROUP_SIZE), "stack_grain_generator");
        true
    }

    /// Layer `i` as flat rects over a background (#2067): a countdown's or
    /// a timer's digits, laid out by `klipp_core::Segments` for the CPU and
    /// here alike. `rects` are `[x0, y0, x1, y1)` in the layer's pixels.
    pub fn stack_layer_segments(&mut self, i: usize, background: [u8; 4], color: [u8; 4], rects: &[[u32; 4]]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() { return false; }
        let (width, height) = self.stack_dims;
        let bytes = (rects.len().max(1) * 16) as u64;
        if self.stack_rects.as_ref().is_none_or(|b| b.size() < bytes) {
            self.stack_rects = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_rects"),
                size: bytes.max(256),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buf = self.stack_rects.as_ref().unwrap();
        if !rects.is_empty() {
            self.upload(buf, 0, bytemuck::cast_slice(rects));
        }
        self.write_params(&SegmentsParams {
            width, height, count: rects.len() as u32, _pad: 0,
            background: background.map(f32::from),
            color: color.map(f32::from),
            to_working: self.authored_to_working,
        });
        self.stack_pass(&self.segments_pipeline, buf, &self.stack_layers[i], (width * height).div_ceil(WORKGROUP_SIZE), "stack_segments");
        true
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct FilmGrainParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) amount: f32,
    /// Grain cells per pixel (`cpu::grain_cells_per_px`), not the size:
    /// the shader multiplies by it, as the CPU does (#1441).
    pub(super) per_px: f32,
    pub(super) color: u32,
    pub(super) shadows: f32,
    pub(super) seed: u32,
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct LivingGradientParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) glow: f32,
    pub(super) grain: f32,
    pub(super) a: [f32; 4],
    pub(super) b: [f32; 4],
    pub(super) dir: [f32; 2],
    pub(super) centre: [f32; 2],
    pub(super) to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GrainGeneratorParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) frame: u32,
    pub(super) _pad: u32,
    pub(super) color: [f32; 4],
    pub(super) cell: f32,
    pub(super) depth: f32,
    pub(super) _p: [f32; 2],
    pub(super) to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SegmentsParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) count: u32,
    pub(super) _pad: u32,
    pub(super) background: [f32; 4],
    pub(super) color: [f32; 4],
    pub(super) to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GeneratorParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) shape: u32,
    pub(super) _pad: u32,
    pub(super) a: [f32; 4],
    pub(super) b: [f32; 4],
    pub(super) dir: [f32; 2],
    pub(super) centre: [f32; 2],
    pub(super) radius: f32,
    pub(super) _pad2: [u32; 3],
}

// Film grain — additive pseudo-random noise, weighted by tonal band.
// Real film grain is photon shot noise, which IS additive in linear
// light; the sRGB version under-represented noise in highlights
// (gamma compresses noise there) and over-represented it in shadows.
// Linear additive matches the physical model.
pub(super) const SHADER_GRAIN: &str = gpu_shader!("generators/grain.wgsl");

// Vignette — multiplicative darkening of the frame's edges. The
// `factor` scalar applies in linear-light so `amount = -1.0` really
// means "half the photons reach the edge" rather than "half the
// sRGB byte value" (which the u8 version did — visibly darker).
// The generator effect (#1746), as `generator::Gen::at` computes it.
// The native living gradient's CPU half-float path, including its
// half-float roundings and its grain (#2073), followed by sRGB decode and
// working-space conversion.
pub(super) const SHADER_LIVING_GRADIENT: &str = gpu_shader!("generators/living_gradient.wgsl");

// The native Grain generator (#2067), as `klipp_core::render_grain`
// draws it: a colour, and each cell's hash, `depth` deep, added and
// truncated to a byte. Then the eight-bit path's decode: the byte as a
// half float, sRGB to linear, into the working space.
pub(super) const SHADER_GRAIN_GENERATOR: &str = gpu_shader!("generators/grain_generator.wgsl");

// Flat rects over a background (#2067): a countdown's or a timer's
// seven-segment digits, laid out on the CPU by `klipp_core::Segments`.
pub(super) const SHADER_SEGMENTS: &str = gpu_shader!("generators/segments.wgsl");

pub(super) const SHADER_GENERATOR: &str = gpu_shader!("generators/generator.wgsl");

pub(super) const SHADER_NOISE_GENERATOR: &str = gpu_shader!("generators/noise_generator.wgsl");

// Film grain — emulsion-like grain in cells of `size` px at 1080p,
// weighted away from the highlights, a new pattern per `seed`. The CPU
// twin uses the same hash and cell arithmetic.
pub(super) const SHADER_FILM_GRAIN: &str = gpu_shader!("generators/film_grain.wgsl");

// Binocular vignette (#989): the frame seen through two eyepieces, the
// CPU twin's arithmetic step for step: the nearest eye's normalised
// distance, a soft edge inside its rim, red and blue split along the
// line from its centre towards the rim, then the outside darkened or
// lightened by `amount`.
// Particles (#1141): `particles::pixel`, step for step. Every number
// that needs trigonometry or a colour conversion arrives worked out.
pub(super) const SHADER_PARTICLES: &str = gpu_shader!("generators/particles.wgsl");
