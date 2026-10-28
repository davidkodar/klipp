//! The effects' uniform params on the GPU (#2890): each pass's `Params`
//! as its shader declares it, plain old data for bytemuck.

use super::*;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CropParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) crop_left: f32,
    pub(super) crop_right: f32,
    pub(super) crop_top: f32,
    pub(super) crop_bottom: f32,
    pub(super) feather: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct TransformParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) anchor_x: f32,
    pub(super) anchor_y: f32,
    pub(super) position_x: f32,
    pub(super) position_y: f32,
    pub(super) cos_r: f32,
    pub(super) sin_r: f32,
    pub(super) inv_sx: f32,
    pub(super) inv_sy: f32,
    pub(super) cos_rx: f32,
    pub(super) sin_rx: f32,
    pub(super) cos_ry: f32,
    pub(super) sin_ry: f32,
    pub(super) focal: f32,
    pub(super) _pad: [u32; 3],
}

impl TransformParams {
    /// The Transform effect's inverse map at `width` × `height`: what its
    /// pass reads, and what the shutter's pass reads for each sample
    /// (#1923), so the two place a picture alike. `None` for any other
    /// effect.
    pub(crate) fn of(effect: &BuiltinEffect, width: u32, height: u32) -> Option<Self> {
        let BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, rotation_x, rotation_y, perspective, .. } = effect else {
            return None;
        };
        let rot = rotation.to_radians();
        let inverse = |s: f32| if s.abs() > f32::EPSILON { 1.0 / s } else { 1.0 };
        let (rx, ry) = (rotation_x.to_radians(), rotation_y.to_radians());
        Some(Self {
            width,
            height,
            anchor_x: *anchor_x * width as f32,
            anchor_y: *anchor_y * height as f32,
            position_x: *position_x,
            position_y: *position_y,
            cos_r: rot.cos(),
            sin_r: rot.sin(),
            inv_sx: inverse(*scale_x),
            inv_sy: inverse(*scale_y),
            cos_rx: rx.cos(),
            sin_rx: rx.sin(),
            cos_ry: ry.cos(),
            sin_ry: ry.sin(),
            focal: if *perspective > 0.001 { width as f32 / *perspective } else { 0.0 },
            _pad: [0; 3],
        })
    }

    /// A sample's row for the shutter's pass: four vec4s.
    pub(super) fn shutter_row(&self) -> [f32; 16] {
        [
            self.anchor_x, self.anchor_y, self.position_x, self.position_y,
            self.cos_r, self.sin_r, self.inv_sx, self.inv_sy,
            self.cos_rx, self.sin_rx, self.cos_ry, self.sin_ry,
            self.focal, 0.0, 0.0, 0.0,
        ]
    }
}

/// The corner pin's inverse map, nine coefficients row-major, from
/// `corner_pin::inverse_map`: the same numbers the CPU samples with.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CornerPinParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) inv: [f32; 9],
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuInputTransformParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) _pad0: u32, pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuMaskCutoutParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) _pad0: u32, pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuScopeAccumParams {
    pub(super) width: u32, pub(super) height: u32, pub(super) display_columns: u32, pub(super) step: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuScopeRenderParams {
    pub(super) out_w: u32, pub(super) out_h: u32, pub(super) num_cols: u32, pub(super) max_count: u32,
}

/// Mask-pipeline frame constants. Per-mask shape data is in the
/// `MaskRecord` storage buffer (binding 2); per-bezier points are
/// pooled in another storage buffer (binding 3). The uniform only
/// holds frame-wide knobs.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuMaskParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mask_count: u32,
    pub(super) _pad: u32,
}

/// One per-mask record in the storage buffer. Layout matches the
/// WGSL `MaskRecord` struct in [`SHADER_MASK_V2`] one-for-one (12
/// `u32`/`f32` slots, then the warp's three rows as `vec4`s at 16-byte
/// offsets = 96 bytes, 16-byte aligned for `array<MaskRecord>`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MaskRecord {
    pub(super) shape_type: u32,     // 0=circle, 1=rect, 2=gradient, 3=bezier, 4=radial gradient
    pub(super) flags: u32,          // bit 0 invert, bit 1 warp, bits 2-3 combine, bits 8-15 opacity (0-255)
    pub(super) bezier_offset: u32,  // index into points buffer (bezier only)
    pub(super) bezier_count: u32,   // number of points (bezier only)
    pub(super) cx: f32, pub(super) cy: f32, pub(super) p1: f32, pub(super) p2: f32,
    pub(super) p3: f32, pub(super) feather: f32, pub(super) feather_inner: f32, pub(super) _pad1: u32,
    pub(super) w0: [f32; 4], pub(super) w1: [f32; 4], pub(super) w2: [f32; 4],  // warp rows (flag bit 1)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuLut3dParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) lut_size: u32,
    pub(super) opacity: f32,
    /// Non-zero: tetrahedral interpolation instead of trilinear.
    pub(super) tetrahedral: u32,
    /// Bit 0: `pre` applies, bit 1: `post` (see `SHADER_LUT3D`).
    pub(super) bracket: u32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
    /// Rows of the working space → LUT input primaries matrix.
    pub(super) pre: [[f32; 4]; 3],
    /// Rows of the LUT output → working space primaries matrix.
    pub(super) post: [[f32; 4]; 3],
}

impl GpuLut3dParams {
    pub(super) fn new(width: u32, height: u32, lut_size: u32, opacity: f32, tetrahedral: bool, bracket: LutBracket) -> Self {
        let rows = |m: Option<[[f32; 3]; 3]>| m.map_or([[0.0; 4]; 3], |m| m.map(|r| [r[0], r[1], r[2], 0.0]));
        Self {
            width, height, lut_size, opacity,
            tetrahedral: tetrahedral as u32,
            bracket: bracket.0.is_some() as u32 | (bracket.1.is_some() as u32) << 1,
            _pad0: 0, _pad1: 0,
            pre: rows(bracket.0),
            post: rows(bracket.1),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuQualifierParams {
    pub(super) width: u32,
    pub(super) height: u32,
    // Qualifier selection
    pub(super) hue_center: f32,     // 0-1 (normalized from 0-360°)
    pub(super) hue_width: f32,      // 0-1
    pub(super) hue_soft: f32,       // 0-1
    pub(super) sat_low: f32,
    pub(super) sat_high: f32,
    pub(super) sat_soft: f32,
    pub(super) lum_low: f32,
    pub(super) lum_high: f32,
    pub(super) lum_soft: f32,
    pub(super) invert: u32,         // 0 or 1
    // Secondary grade
    pub(super) sec_exposure: f32,
    pub(super) sec_saturation: f32,
    pub(super) sec_hue_shift: f32,  // radians
    pub(super) sec_temperature: f32,
    // Matte preview
    pub(super) matte_preview: u32,
    /// 0 HSL, 1 RGB (#895).
    pub(super) mode: u32,
    pub(super) _pad1: u32,
    pub(super) _pad2: u32,
    /// The RGB mode's ranges: red, green, blue low and high, the soft edge.
    pub(super) rgb: [f32; 7],
    pub(super) _pad3: f32,
    /// The 3D mode's volumes (#895): each centre (its `w` 1 when there is
    /// one) and inverse spread by rows, and the reach and its soft edge.
    pub(super) keep_mean: [f32; 4],
    pub(super) keep_inv: [[f32; 4]; 3],
    pub(super) remove_mean: [f32; 4],
    pub(super) remove_inv: [[f32; 4]; 3],
    pub(super) reach: [f32; 4],
}

impl GpuQualifierParams {
    /// The mode and RGB ranges `rgb` gives (#895): HSL's, with the RGB
    /// ranges unused, for `None`.
    pub(super) fn rgb_mode(rgb: Option<[f32; 7]>) -> (u32, [f32; 7]) {
        match rgb {
            Some(r) => (1, r),
            None => (0, [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.1]),
        }
    }

    /// The mode, ranges and volumes a key's shape gives: 3D for a
    /// volume, else RGB's or HSL's.
    pub(super) fn shape(rgb: Option<[f32; 7]>, volume: Option<QualifierVolume>) -> (u32, [f32; 7], [[f32; 4]; 9]) {
        let (mut mode, ranges) = Self::rgb_mode(rgb);
        let mut v = [[0.0f32; 4]; 9];
        if let Some(q) = volume {
            mode = 2;
            let mut put = |at: usize, k: Option<crate::cpu_grade::KeyVolume>| {
                if let Some(k) = k {
                    v[at] = [k.mean[0], k.mean[1], k.mean[2], 1.0];
                    for r in 0..3 {
                        v[at + 1 + r] = [k.inv[r][0], k.inv[r][1], k.inv[r][2], 0.0];
                    }
                }
            };
            put(0, q.keep);
            put(4, q.remove);
            v[8] = [q.radius, q.soft, 0.0, 0.0];
        }
        (mode, ranges, v)
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuLumaKeyParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) low: f32,
    pub(super) high: f32,
    pub(super) soft: f32,
    pub(super) invert: u32,
    /// 1: the matte multiplies the picture's alpha, the colour kept (the
    /// Luma Key clip effect, #1638); 0: the greyscale matte (the node).
    pub(super) into_alpha: u32,
    pub(super) _pad1: u32,
    /// Working-space luma coefficients, `klipp_color::linear::luma_coefs`.
    /// A vec4 because WGSL aligns vec3 to 16 bytes anyway; `.w` unused.
    pub(super) luma: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuTemporalDenoiseParams {
    pub(super) width: u32,
    pub(super) height: u32,
    /// Neighbours bound (0 to 2); a slot past it is bound but not read.
    pub(super) count: u32,
    pub(super) _pad0: u32,
    /// `temporal_denoise::denominator(threshold)`, worked out on the host.
    pub(super) denom: f32,
    pub(super) strength: f32,
    pub(super) _pad1: u32,
    pub(super) _pad2: u32,
    /// The guard's luma weights, as the caller passes them; `.w` unused.
    pub(super) luma: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuDifferenceKeyParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tolerance: f32,
    pub(super) soft: f32,
    pub(super) invert: u32,
    /// 0 = RGB (Chebyshev), 1 = weighted luminance. Matches
    /// `klipp_core::DifferenceKeyMatchSpace`'s order; the host converts.
    pub(super) match_space: u32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
    /// Working-space luminance row; `.w` unused. Both sides of the
    /// comparison are linear working-space values, so Rec.709's row
    /// would mis-weight an ACEScg project.
    pub(super) luma: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuSoftClipParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) threshold: f32,
    pub(super) knee: f32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
    pub(super) _pad2: u32,
    pub(super) _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuGrainParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) amount: f32,
    pub(super) size: f32,
    pub(super) shadows: f32,
    pub(super) highlights: f32,
    pub(super) frame_seed: u32,
    pub(super) monochrome: u32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad_luma: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct AlphaBlurParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) radius: u32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct AlphaDilateParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) radius: u32,
    pub(super) radius_sq: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct InvertParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mix: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CstParams {
    pub(super) width: u32,
    pub(super) height: u32,
    /// Bit 0: read through the input curve, 1: write through the output
    /// curve, 2: convert by the matrix.
    pub(super) flags: u32,
    pub(super) _pad: u32,
    /// The matrix's rows.
    pub(super) m: [[f32; 4]; 3],
}

impl CstParams {
    pub(super) fn new(width: u32, height: u32, t: &crate::colour_transform::CstTables) -> Self {
        let m = t.matrix.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        Self {
            width,
            height,
            flags: t.decode.is_some() as u32 | (t.encode.is_some() as u32) << 1 | (t.matrix.is_some() as u32) << 2,
            _pad: 0,
            m: m.map(|r| [r[0], r[1], r[2], 0.0]),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct PosterizeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    /// Clamped to [2, 256] (`PosterizeLadder`).
    pub(super) levels: f32,
    /// How many of the table's above-white edges are edges.
    pub(super) above_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MosaicParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) block_size: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct EdgeDetectParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) strength: f32,
    pub(super) mix: f32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad_luma: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ChannelMixerParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rr: f32,
    pub(super) rg: f32,
    pub(super) rb: f32,
    pub(super) gr: f32,
    pub(super) gg: f32,
    pub(super) gb: f32,
    pub(super) br: f32,
    pub(super) bg: f32,
    pub(super) bb: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct DirectionalBlurParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) dx: f32,
    pub(super) dy: f32,
    pub(super) samples: u32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
    pub(super) _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct EmbossParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) dx: f32,
    pub(super) dy: f32,
    pub(super) strength: f32,
    pub(super) mix: f32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MirrorParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) horizontal: u32,
    pub(super) vertical: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct LensDistortionParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) distortion: f32,
    pub(super) scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct DilateErodeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) radius: u32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct DenoiseParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) spatial_sigma: f32,
    /// The range sigma, on the luma difference, for smoothing the luma:
    /// the Luma slider. 0 leaves the luma as it is (#1039).
    pub(super) luma_sigma: f32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    /// The largest kernel radius, in pixels of the buffer: 10 at the
    /// sequence's size, scaled with the render size for the grade node.
    pub(super) max_kernel: u32,
    /// The range sigma, also on the luma difference, for smoothing the
    /// chroma `rgb − Y`: the Chroma slider. 0 leaves the chroma as it
    /// is (#1039). Before, the two were averaged into one sigma.
    pub(super) chroma_sigma: f32,
    pub(super) _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GammaParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) inv_gamma: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct HSVAdjustParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) hue_shift: f32,
    pub(super) saturation: f32,
    pub(super) value: f32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
    pub(super) _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct NoiseGeneratorParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) amount: f32,
    pub(super) monochrome: u32,
    pub(super) seed: u32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
    pub(super) _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MedianFilterParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) radius: u32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct RadialBlurParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) center_x: f32,
    pub(super) center_y: f32,
    pub(super) amount: f32,
    pub(super) samples: u32,
    pub(super) mode: u32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct LevelsParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) in_black: f32,
    pub(super) in_white: f32,
    pub(super) inv_gamma: f32,
    pub(super) out_black: f32,
    pub(super) out_white: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct EqualizeParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) strength: f32,
    pub(super) _pad0: u32,
    /// The working space's luma weights, and a pad.
    pub(super) luma: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct LetterboxParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) aspect_ratio: f32,
    pub(super) bar_color: u32,
    pub(super) opacity: f32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
    pub(super) _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct RscParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) strength: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct StabilizationParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) crop_zoom: f32,
    pub(super) _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ChromaticAberrationParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) red_shift: f32,
    pub(super) blue_shift: f32,
    pub(super) center_x: f32,
    pub(super) center_y: f32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MetalShotParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) op: u32,
    pub(super) scale: u32,
    pub(super) roi_x: u32,
    pub(super) roi_y: u32,
    pub(super) roi_w: u32,
    pub(super) roi_h: u32,
    pub(super) small_w: u32,
    pub(super) small_h: u32,
    pub(super) base: u32,
    pub(super) radius: u32,
    pub(super) tint_shot: [f32; 4],
}
