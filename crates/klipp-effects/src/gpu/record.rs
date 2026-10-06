//! Built-in effects recorded into a caller's encoder (node-graph M2
//! step 7): no submit of their own, so a run of effects is one submit.
//!
//! The contract every recorded arm keeps:
//! - **Never in place.** `src` is only read. The result is in `dst`; an
//!   arm of several passes ping-pongs between `dst` and `scratch` and
//!   ends in `dst`. So a mix after the effect can still read its input.
//! - **Linear in, linear out.** An effect that works on the display
//!   scale (invert, posterize, gamma, HSV, levels) encodes and decodes
//!   inside its own pass; [`converts_inside`] names them, so a caller
//!   adds no conversion around them.
//! - **Parameters arrive final.** Animation and render scale are the
//!   caller's; the values are used as given.
//! - **Straight alpha** in and out.
//! - Each pass claims its own uniform slot from the caller's
//!   [`ParamSlots`]; the caller submits before resetting them.

use super::*;
use crate::registry::ColourDomain;

/// Where recorded passes go: the caller's encoder and uniform slots.
#[doc(hidden)]
pub struct EffectRecording<'a> {
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub slots: &'a mut ParamSlots,
}

/// What [`GpuEffectsPipeline::record_effect`] did.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Passes recorded; the result will be in `dst`.
    Done,
    /// The effect changes nothing at these parameters: nothing recorded,
    /// the result is `src`.
    Identity,
    /// Not recorded: an arm not ported yet, or one that must break the
    /// segment (Equalize reads the frame back). The caller submits what
    /// it has recorded and runs the effect another way.
    NotRecordable,
    /// The slots left are fewer than the effect needs. Nothing recorded:
    /// the caller submits, resets the slots and asks again.
    OutOfSlots,
}

/// Uniform slots `effect` claims when recorded, or `None` when it isn't
/// recordable. So a plan can size its segments before it records.
#[doc(hidden)]
pub fn slots_needed(effect: &BuiltinEffect) -> Option<usize> {
    match effect {
        BuiltinEffect::Opacity { .. }
        | BuiltinEffect::Invert { .. }
        | BuiltinEffect::Posterize { .. }
        | BuiltinEffect::ChannelMixer { .. }
        | BuiltinEffect::Gamma { .. }
        | BuiltinEffect::HSVAdjust { .. }
        | BuiltinEffect::Levels { .. }
        | BuiltinEffect::Vibrance { .. }
        | BuiltinEffect::ColorFade { .. }
        | BuiltinEffect::Crop { .. }
        | BuiltinEffect::Transform { .. }
        | BuiltinEffect::Mirror { .. }
        | BuiltinEffect::LensDistortion { .. }
        | BuiltinEffect::Letterbox { .. } => Some(1),
        _ => None,
    }
}

/// The scale a recorded effect works on inside its own pass, when that
/// isn't linear light. Its input and output are linear all the same.
#[doc(hidden)]
pub fn converts_inside(effect: &BuiltinEffect) -> Option<ColourDomain> {
    match effect {
        BuiltinEffect::Invert { .. }
        | BuiltinEffect::Posterize { .. }
        | BuiltinEffect::Gamma { .. }
        | BuiltinEffect::HSVAdjust { .. }
        | BuiltinEffect::Levels { .. }
        | BuiltinEffect::Equalize { .. } => Some(ColourDomain::DisplayEncoded),
        _ => None,
    }
}

impl GpuEffectsPipeline {
    /// Record `effect` from `src` into `dst`, `width` × `height`, into
    /// `rec`; see the module's contract. `scratch` is a third buffer of
    /// the frame's size for arms of several passes.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn record_effect(
        &self,
        effect: &BuiltinEffect,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        _scratch: Option<&wgpu::Buffer>,
        width: u32,
        height: u32,
        rec: &mut EffectRecording<'_>,
    ) -> Recorded {
        let Some(needed) = slots_needed(effect) else { return Recorded::NotRecordable };
        if self.is_device_lost() {
            return Recorded::NotRecordable;
        }
        if rec.slots.remaining() < needed {
            return Recorded::OutOfSlots;
        }
        // One pass each: its pipeline, and its parameters in a slot of
        // their own (`slots_needed` was checked, so the claim succeeds).
        let q = &self.queue;
        let (pipeline, slot) = match effect {
            BuiltinEffect::Opacity { value } => {
                if (*value - 1.0).abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &OpacityParams { width, height, value: *value, _pad: 0 });
                (&self.opacity_pipeline, slot)
            }
            BuiltinEffect::Invert { mix } => {
                if (*mix).abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &InvertParams { width, height, mix: *mix, _pad: 0 });
                (&self.invert_pipeline, slot)
            }
            BuiltinEffect::Posterize { levels } => {
                // At least two, as on the CPU (#1020).
                let slot = rec.slots.claim(q, &PosterizeParams { width, height, levels: levels.max(2.0), _pad: 0 });
                (&self.posterize_pipeline, slot)
            }
            BuiltinEffect::ChannelMixer {
                red_red, red_green, red_blue,
                green_red, green_green, green_blue,
                blue_red, blue_green, blue_blue,
            } => {
                let slot = rec.slots.claim(q, &ChannelMixerParams {
                    width, height,
                    rr: *red_red, rg: *red_green, rb: *red_blue,
                    gr: *green_red, gg: *green_green, gb: *green_blue,
                    br: *blue_red, bg: *blue_green, bb: *blue_blue,
                    _pad: 0,
                });
                (&self.channel_mixer_pipeline, slot)
            }
            BuiltinEffect::Gamma { gamma } => {
                if (*gamma - 1.0).abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &GammaParams { width, height, inv_gamma: 1.0 / *gamma, _pad: 0 });
                (&self.gamma_pipeline, slot)
            }
            BuiltinEffect::HSVAdjust { hue_shift, saturation, value } => {
                if hue_shift.abs() < f32::EPSILON && (*saturation - 1.0).abs() < f32::EPSILON && (*value - 1.0).abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &HSVAdjustParams {
                    width, height,
                    hue_shift: *hue_shift,
                    saturation: *saturation,
                    value: *value,
                    _pad: 0, _pad2: 0, _pad3: 0,
                });
                (&self.hsv_adjust_pipeline, slot)
            }
            BuiltinEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
                if *in_black <= 0.0 && (*in_white - 1.0).abs() < f32::EPSILON
                    && (*gamma - 1.0).abs() < f32::EPSILON
                    && *out_black <= 0.0 && (*out_white - 1.0).abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &LevelsParams {
                    width, height,
                    in_black: *in_black,
                    in_white: *in_white,
                    inv_gamma: if gamma.abs() > f32::EPSILON { 1.0 / *gamma } else { 1.0 },
                    out_black: *out_black,
                    out_white: *out_white,
                    _pad: 0,
                });
                (&self.levels_pipeline, slot)
            }
            BuiltinEffect::Vibrance { amount } => {
                let slot = rec.slots.claim(q, &GpuVibranceParams {
                    width, height, amount: *amount, _pad: 0,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0,
                });
                (&self.vibrance_pipeline, slot)
            }
            BuiltinEffect::ColorFade { color, progress, .. } => {
                let t = progress.clamp(0.0, 1.0);
                if t >= 0.9999 {
                    return Recorded::Identity;
                }
                let lin = |c: u8| crate::cpu::decode(c as f32 / 255.0);
                let slot = rec.slots.claim(q, &ColorFadeParams {
                    width, height, progress: t, _pad: 0,
                    color: [lin(color[0]), lin(color[1]), lin(color[2]), color[3] as f32 / 255.0],
                });
                (&self.color_fade_pipeline, slot)
            }
            BuiltinEffect::Crop {
                left,
                right,
                top,
                bottom,
                feather,
            } => {
                if *left <= 0.0 && *right <= 0.0 && *top <= 0.0 && *bottom <= 0.0 {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &CropParams {
                    width,
                    height,
                    crop_left: *left * width as f32,
                    crop_right: (1.0 - *right) * width as f32,
                    crop_top: *top * height as f32,
                    crop_bottom: (1.0 - *bottom) * height as f32,
                    feather: *feather,
                    _pad: 0,
                });
                (&self.crop_pipeline, slot)
            }
            BuiltinEffect::Transform {
                position_x,
                position_y,
                scale_x,
                scale_y,
                rotation,
                anchor_x,
                anchor_y,
                rotation_x,
                rotation_y,
                perspective,
                focal_ref: _,
            } => {
                if *position_x == 0.0
                    && *position_y == 0.0
                    && (*scale_x - 1.0).abs() < f32::EPSILON
                    && (*scale_y - 1.0).abs() < f32::EPSILON
                    && rotation.abs() < f32::EPSILON
                    && rotation_x.abs() < f32::EPSILON
                    && rotation_y.abs() < f32::EPSILON
                {
                    return Recorded::Identity;
                }
                let rot_rad = rotation.to_radians();
                let cos_r = rot_rad.cos();
                let sin_r = rot_rad.sin();
                let inv_sx = if scale_x.abs() > f32::EPSILON {
                    1.0 / scale_x
                } else {
                    1.0
                };
                let inv_sy = if scale_y.abs() > f32::EPSILON {
                    1.0 / scale_y
                } else {
                    1.0
                };
                let rx_rad = rotation_x.to_radians();
                let ry_rad = rotation_y.to_radians();
                let focal = if *perspective > 0.001 { width as f32 / *perspective } else { 0.0 };
                let slot = rec.slots.claim(q, &TransformParams {
                    width,
                    height,
                    anchor_x: *anchor_x * width as f32,
                    anchor_y: *anchor_y * height as f32,
                    position_x: *position_x,
                    position_y: *position_y,
                    cos_r,
                    sin_r,
                    inv_sx,
                    inv_sy,
                    cos_rx: rx_rad.cos(),
                    sin_rx: rx_rad.sin(),
                    cos_ry: ry_rad.cos(),
                    sin_ry: ry_rad.sin(),
                    focal,
                    _pad: [0; 3],
                });
                (&self.transform_pipeline, slot)
            }
            BuiltinEffect::Mirror { horizontal, vertical } => {
                if *horizontal == 0 && *vertical == 0 {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &MirrorParams {
                    width, height,
                    horizontal: *horizontal,
                    vertical: *vertical,
                });
                (&self.mirror_pipeline, slot)
            }
            BuiltinEffect::LensDistortion { distortion, scale } => {
                if distortion.abs() < f32::EPSILON && (*scale - 1.0).abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &LensDistortionParams {
                    width, height,
                    distortion: *distortion,
                    scale: *scale,
                });
                (&self.lens_distortion_pipeline, slot)
            }
            BuiltinEffect::Letterbox { aspect_ratio, bar_color, opacity } => {
                let slot = rec.slots.claim(q, &LetterboxParams {
                    width, height,
                    aspect_ratio: *aspect_ratio,
                    bar_color: *bar_color,
                    opacity: *opacity,
                    _pad0: 0,
                    _pad1: 0,
                    _pad2: 0,
                });
                (&self.letterbox_pipeline, slot)
            }
            _ => return Recorded::NotRecordable,
        };
        let slot = slot.expect("slots_needed was checked");
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("recorded_effect"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: dst.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: rec.slots.buffer(slot).as_entire_binding() },
            ],
        });
        Self::record_compute_dispatch(rec.encoder, pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
        Recorded::Done
    }
}
