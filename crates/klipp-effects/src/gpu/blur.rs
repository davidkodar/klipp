//! Blur and detail on the GPU (#2890): the blurs (box, directional,
//! radial, focus, tilt-shift), sharpen, median, denoise, dilate and erode,
//! edge detect and emboss, with their params and shaders.

use super::*;

impl GpuEffectsPipeline {
    /// Apply bilateral denoise on GPU. Used by the color grading Denoise node.
    pub fn apply_denoise_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        spatial_sigma: f32,
        luma_sigma: f32,
        chroma_sigma: f32,
        max_kernel: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&DenoiseParams {
            width, height, spatial_sigma, luma_sigma,
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], max_kernel,
            chroma_sigma, _pad: [0; 3],
        });
        self.dispatch(&self.pipelines.denoise_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Dispatch-only bilateral denoise pass.
    pub fn apply_denoise_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        spatial_sigma: f32,
        luma_sigma: f32,
        chroma_sigma: f32,
        max_kernel: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &DenoiseParams { width, height, spatial_sigma, luma_sigma, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], max_kernel, chroma_sigma, _pad: [0; 3] });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.pipelines.denoise_pipeline, bind_group, workgroups);
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct FocusBlurParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) vertical: u32,
    pub(super) band: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct TiltShiftParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) center: f32,
    pub(super) band: f32,
    pub(super) softness: f32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
    pub(super) _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BlurParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) radius: i32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SharpenParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) amount: f32,
    pub(super) _pad: u32,
}

// Horizontal blur. Averages linear-light RGB — the gamma-encoded
// version underestimated midtone brightness and made edges look
// unnaturally dark. unpack decodes sRGB u8 → linear f32 [0,1];
// pack encodes linear f32 → sRGB u8 at the 8-bit storage boundary.
// Alpha stays linear per sRGB convention.
pub(super) const SHADER_BLUR_H: &str = gpu_shader!("blur/blur_h.wgsl");

pub(super) const SHADER_BLUR_V: &str = gpu_shader!("blur/blur_v.wgsl");

pub(super) const SHADER_SHARPEN: &str = gpu_shader!("blur/sharpen.wgsl");

// Edge detect — Sobel filter on luma. BT.709 coefficients are the
// standard for linear-light luma; using them on sRGB-encoded bytes
// (as the old version did) misweighed channels. Linear Sobel
// produces slightly different edge magnitudes that are more
// consistent with the physical gradient.
pub(super) const SHADER_EDGE_DETECT: &str = gpu_shader!("blur/edge_detect.wgsl");

// Directional blur — averages along a `(dx, dy)` line. Linear-light
// averaging so midtones don't darken through the trail, and premultiplied
// so a picture over transparency smears at its coverage (#2021): the
// colour is the alpha-weighted mean, back to straight by the summed alpha
// (`UNPREMULTIPLY_MIN_ALPHA`, as the shutter's mean, #2017). Every tap
// opaque, the plain mean as before, to the bit.
pub(super) const SHADER_DIRECTIONAL_BLUR: &str = concat!(
    // Two parts around the alpha floor, which the shader uses inside an
    // expression: an include can only stand for whole lines (#2890).
    gpu_shader!("blur/directional_blur.1.wgsl"),
    unpremultiply_min_alpha_wgsl!(),
    gpu_shader!("blur/directional_blur.2.wgsl"),
);

// Emboss — relief effect via directional difference. Same math as
// the sRGB version but pivoted at linear 0.18 (perceptual mid-grey)
// and with all differencing in linear. Result is visually close to
// the old version (the 0.5 sRGB vs 0.18 linear pivot differ by ≈
// one code value on neutral output).
pub(super) const SHADER_EMBOSS: &str = gpu_shader!("blur/emboss.wgsl");

pub(super) const SHADER_DILATE: &str = gpu_shader!("blur/dilate.wgsl");

pub(super) const SHADER_ERODE: &str = gpu_shader!("blur/erode.wgsl");

// Denoise — bilateral filter in linear light that smooths luma `Y`
// and chroma `rgb − Y` separately, each with its own range sigma on
// the luma difference: Luma slider and Chroma slider (#1039).
// `cpu_grade::bilateral_denoise` is the CPU twin and documents it.
pub(super) const SHADER_DENOISE: &str = gpu_shader!("blur/denoise.wgsl");

// Tilt-shift blend — the original inside the sharp band, the blurred
// frame beyond it, over a smoothstep ramp of `softness`.
// Focus blur blend (#592): the blurred layer mixed over the original
// by how far the pixel is from the band around the frame's middle,
// every channel, alpha too. `cpu::focus_blur_mix` is the CPU twin.
pub(super) const SHADER_FOCUS_BLUR_BLEND: &str = gpu_shader!("blur/focus_blur_blend.wgsl");

// A focus blur's copy of the layer, premultiplied and keeping its alpha,
// for the blur passes (the frame's own premultiply ends at alpha 1).
pub(super) const SHADER_FOCUS_BLUR_PREMULTIPLY: &str = gpu_shader!("blur/focus_blur_premultiply.wgsl");

pub(super) const SHADER_TILT_SHIFT_BLEND: &str = gpu_shader!("blur/tilt_shift_blend.wgsl");

pub(super) const SHADER_MEDIAN_FILTER: &str = gpu_shader!("blur/median_filter.wgsl");

// Radial blur (zoom or spin). Averages samples along a radial or
// rotational path, in linear light. Zoom spreads the samples over
// ±amount % of each pixel's distance from the centre; spin over an arc
// of `amount` degrees. (Both amounts used to be divided by the sample
// count and the distance again, so the presets moved pixels by less
// than one — no visible blur.)
pub(super) const SHADER_RADIAL_BLUR: &str = gpu_shader!("blur/radial_blur.wgsl");
