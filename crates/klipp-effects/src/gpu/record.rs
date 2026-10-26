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
    /// segment. The caller submits what it has recorded and runs the
    /// effect another way.
    NotRecordable,
    /// The slots left are fewer than the effect needs. Nothing recorded:
    /// the caller submits, resets the slots and asks again.
    OutOfSlots,
}

/// Uniform slots `effect` claims when recorded, at most, or `None` when
/// it isn't recordable. So a plan can size its segments before it records.
#[doc(hidden)]
pub fn slots_needed(effect: &BuiltinEffect) -> Option<usize> {
    // A flicker is the plain effect it is at its time (#989).
    if let Some(plain) = crate::flicker::as_plain(effect) {
        return slots_needed(&plain);
    }
    // A box blur's horizontal and vertical passes share one slot: the
    // same parameters.
    let boxes = |radius: f32| crate::blur::boxes_for_gaussian(radius).iter().filter(|r| **r != 0).count();
    match effect {
        // Done elsewhere, or not by the GPU at all: they pass the frame
        // through here, as they did, without breaking a run.
        BuiltinEffect::SpeedRamp { .. } | BuiltinEffect::TemporalDenoise { .. } | BuiltinEffect::Frei0r { .. } | BuiltinEffect::Merge { .. } | BuiltinEffect::MediaSource { .. } | BuiltinEffect::ClipBelow {} | BuiltinEffect::GradeCompound {} => Some(0),
        BuiltinEffect::RollingShutterCorrection { .. } | BuiltinEffect::VideoStabilization { .. } => Some(1),
        // A bright or copied layer, three box passes over it (zero boxes
        // too, as before) and the blend: see `record_light_layer`.
        BuiltinEffect::Bloom { .. } | BuiltinEffect::Diffusion { .. } | BuiltinEffect::FilmHalation { .. } => Some(5),
        // The layer passes (`gpu/layer.rs`); a spill's count depends on
        // the frame's width, so this is its most. A run of spills is
        // `record_spill_run`'s.
        BuiltinEffect::RoundedCorners { .. } => Some(1),
        BuiltinEffect::FillBars { .. } => Some(GpuEffectsPipeline::FILL_SLOTS),
        BuiltinEffect::DropShadow { .. } | BuiltinEffect::OuterGlow { .. } => Some(5),
        // Glow's layer is premultiplied first (#2703).
        BuiltinEffect::Glow { .. } => Some(5),
        BuiltinEffect::LightWrap { .. } => Some(4),
        BuiltinEffect::GaussianBlur { radius } => Some(boxes(*radius)),
        BuiltinEffect::Denoise { .. } => Some(1),
        BuiltinEffect::TiltShift { radius, .. } => Some(boxes(*radius) + 1),
        BuiltinEffect::FocusBlur { radius, .. } => Some(boxes(*radius) + 2),
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
        | BuiltinEffect::Displace { .. }
        | BuiltinEffect::Reflection { .. }
        | BuiltinEffect::Letterbox { .. }
        | BuiltinEffect::Dilate { .. }
        | BuiltinEffect::DirectionalBlur { .. }
        | BuiltinEffect::EdgeDetect { .. }
        | BuiltinEffect::Emboss { .. }
        | BuiltinEffect::Erode { .. }
        | BuiltinEffect::Mosaic { .. }
        | BuiltinEffect::RadialBlur { .. }
        | BuiltinEffect::Sharpen { .. }
        | BuiltinEffect::MedianFilter { .. }
        | BuiltinEffect::ChromaKey { .. }
        | BuiltinEffect::LumaKey { .. }
        | BuiltinEffect::Deflicker { .. }
        | BuiltinEffect::FilmGrain { .. }
        | BuiltinEffect::NoiseGenerator { .. }
        | BuiltinEffect::OldFilm { .. }
        | BuiltinEffect::Vhs { .. }
        | BuiltinEffect::CornerPin { .. }
        | BuiltinEffect::AnamorphicStreak { .. }
        | BuiltinEffect::ChromaticAberration { .. }
        | BuiltinEffect::LensFlare { .. }
        | BuiltinEffect::LightRays { .. }
        | BuiltinEffect::Vignette { .. }
        | BuiltinEffect::BinocularVignette { .. }
        | BuiltinEffect::Particles { .. }
        | BuiltinEffect::Lut3D { .. }
        | BuiltinEffect::ColourSpaceTransform { .. }
        | BuiltinEffect::PictureTransform { .. }
        | BuiltinEffect::Generator { .. }
        // Its histogram, CDF and map passes share the one slot (#1215).
        | BuiltinEffect::Equalize { .. } => Some(1),
        _ => None,
    }
}

/// The scale a recorded effect works on inside its own pass, when that
/// isn't linear light. Its input and output are linear all the same.
#[doc(hidden)]
pub fn converts_inside(effect: &BuiltinEffect) -> Option<ColourDomain> {
    if let Some(plain) = crate::flicker::as_plain(effect) {
        return converts_inside(&plain);
    }
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
        scratch: Option<&wgpu::Buffer>,
        width: u32,
        height: u32,
        rec: &mut EffectRecording<'_>,
    ) -> Recorded {
        if slots_needed(effect).is_none() || self.is_device_lost() || self.refuse_recording {
            return Recorded::NotRecordable;
        }
        // A picture transform is the Transform effect's pass (#1746).
        if let Some(t) = effect.picture_transform_as_transform() {
            return self.record_effect(&t, src, dst, scratch, width, height, rec);
        }
        // A flicker records as the plain effect it is now (#989).
        if let Some(plain) = crate::flicker::as_plain(effect) {
            return self.record_effect(&plain, src, dst, scratch, width, height, rec);
        }
        // Each arm checks its identity before its slots, so an effect that
        // records nothing never asks for a refill.
        let q = &self.queue;
        if let Some(outcome) = self.record_multi_pass(effect, src, dst, scratch, width, height, rec) {
            return outcome;
        }
        // Posterize reads its ladder's edges from its slot's table, the
        // ones the CPU finds bands against (#1443).
        if let BuiltinEffect::Posterize { levels } = effect {
            // The ladder is made and uploaded only when the slot doesn't
            // hold this one already (`claim_with_table`, keyed by levels).
            let levels = crate::cpu::PosterizeLadder::clamped(*levels);
            let params = PosterizeParams { width, height, levels, above_count: crate::cpu::PosterizeLadder::above_count(levels) };
            let table = || {
                let ladder = crate::cpu::PosterizeLadder::new(levels);
                ladder.inside.iter().chain(&ladder.above).copied().collect()
            };
            let Some(slot) = rec.slots.claim_with_table(q, &params, u64::from(levels.to_bits()), table) else { return Recorded::OutOfSlots };
            self.record_with_table(rec, &self.posterize_pipeline, src, dst, slot, width, height);
            return Recorded::Done;
        }
        // Equalize (#1215): the frame's histogram, its CDF, then the map
        // through it, all in this encoder. The histogram is cleared here,
        // before each Equalize counts, so a second one in the chain starts
        // from nothing.
        if let BuiltinEffect::Equalize { strength } = effect {
            let params = EqualizeParams { width, height, strength: *strength, _pad0: 0, luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0] };
            let Some(slot) = rec.slots.claim(q, &params) else { return Recorded::OutOfSlots };
            let params = rec.slots.buffer(slot);
            let bind = |label, a: &wgpu::Buffer, b: &wgpu::Buffer, c: &wgpu::Buffer| {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &self.huesat_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: a.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: b.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: c.as_entire_binding() },
                    ],
                })
            };
            let (hist, cdf) = (&self.equalize_hist_buf, &self.equalize_cdf_buf);
            let pixels = (width * height).div_ceil(WORKGROUP_SIZE);
            rec.encoder.clear_buffer(hist, 0, None);
            Self::record_compute_dispatch(rec.encoder, &self.equalize_histogram_pipeline, &bind("equalize_histogram", src, hist, cdf), pixels);
            Self::record_compute_dispatch(rec.encoder, &self.equalize_cdf_pipeline, &bind("equalize_cdf", hist, cdf, hist), 1);
            Self::record_compute_dispatch(rec.encoder, &self.equalize_pipeline, &bind("equalize_map", src, dst, cdf), pixels);
            return Recorded::Done;
        }
        if let BuiltinEffect::Lut3D { prepared, opacity, tetrahedral, .. } = effect {
            // Nothing prepared (the file unread) passes, as on the CPU.
            let Some(lut) = prepared.as_ref().filter(|l| *opacity > 0.0 && l.size >= 2 && l.data.len() >= l.size.pow(3) * 3) else {
                return Recorded::Identity;
            };
            let params = GpuLut3dParams::new(width, height, lut.size as u32, *opacity, *tetrahedral, (lut.pre, lut.post));
            let Some(slot) = rec.slots.claim(q, &params) else { return Recorded::OutOfSlots };
            let table = self.effect_lut_buffer(&lut.0);
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("recorded_lut"),
                layout: &self.huesat_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: dst.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: rec.slots.buffer(slot).as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: table.as_entire_binding() },
                ],
            });
            Self::record_compute_dispatch(rec.encoder, &self.lut3d_pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
            return Recorded::Done;
        }
        // The transform's two curve tables ride in its slot's table, keyed
        // by the curves (the tables are the curves'), apart from
        // Posterize's keys by the top bit.
        if let BuiltinEffect::ColourSpaceTransform { prepared, input_curve, output_curve, .. } = effect {
            let Some(t) = prepared.as_ref().filter(|t| t.matrix.is_some() || t.decode.is_some() || t.encode.is_some()) else {
                return Recorded::Identity;
            };
            let used = |on: bool, curve: u32| if on { u64::from(curve) + 1 } else { 0 };
            let key = 1 << 63 | used(t.decode.is_some(), *input_curve) << 16 | used(t.encode.is_some(), *output_curve);
            let table = || crate::colour_transform::slot_table(t);
            let Some(slot) = rec.slots.claim_with_table(q, &CstParams::new(width, height, t), key, table) else { return Recorded::OutOfSlots };
            self.record_with_table(rec, &self.cst_pipeline, src, dst, slot, width, height);
            return Recorded::Done;
        }
        // One pass each: its pipeline, and its parameters in a slot of
        // their own, claimed before anything is recorded.
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
                let _ = (anchor_x, anchor_y, perspective);
                let params = TransformParams::of(effect, width, height).expect("a Transform");
                let slot = rec.slots.claim(q, &params);
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
            BuiltinEffect::Displace { amount, size, speed, direction, seed, time } => {
                if amount.abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &crate::gpu::DisplaceParams {
                    width, height,
                    amount: *amount, size: *size, speed: *speed,
                    direction: *direction, seed: *seed, time: *time,
                });
                (&self.displace_pipeline, slot)
            }
            BuiltinEffect::Reflection { .. } => {
                let Some(params) = effect.reflection_params(width, height).filter(|_| !crate::is_identity_effect(effect)) else {
                    return Recorded::Identity;
                };
                let slot = rec.slots.claim(q, &params);
                (&self.reflection_pipeline, slot)
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
            BuiltinEffect::Dilate { radius } => {
                let slot = rec.slots.claim(q, &DilateErodeParams {
                    width, height,
                    radius: (*radius).clamp(1, 10),
                    _pad: 0,
                });
                (&self.dilate_pipeline, slot)
            }
            BuiltinEffect::DirectionalBlur { angle, distance, samples } => {
                if *distance < 0.5 {
                    return Recorded::Identity;
                }
                let samples_clamped = (*samples).clamp(3, 32);
                let angle_rad = angle.to_radians();
                let slot = rec.slots.claim(q, &DirectionalBlurParams {
                    width, height,
                    dx: angle_rad.cos() * *distance / samples_clamped as f32,
                    dy: angle_rad.sin() * *distance / samples_clamped as f32,
                    samples: samples_clamped,
                    _pad: 0, _pad2: 0, _pad3: 0,
                });
                (&self.directional_blur_pipeline, slot)
            }
            BuiltinEffect::EdgeDetect { strength, mix } => {
                let slot = rec.slots.claim(q, &EdgeDetectParams {
                    width, height,
                    strength: *strength,
                    mix: *mix,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
                });
                (&self.edge_detect_pipeline, slot)
            }
            BuiltinEffect::Emboss { angle, strength, mix } => {
                let angle_rad = angle.to_radians();
                let slot = rec.slots.claim(q, &EmbossParams {
                    width, height,
                    dx: angle_rad.cos(),
                    dy: angle_rad.sin(),
                    strength: *strength,
                    mix: *mix,
                    _pad0: 0, _pad1: 0,
                });
                (&self.emboss_pipeline, slot)
            }
            BuiltinEffect::Erode { radius } => {
                let slot = rec.slots.claim(q, &DilateErodeParams {
                    width, height,
                    radius: (*radius).clamp(1, 10),
                    _pad: 0,
                });
                (&self.erode_pipeline, slot)
            }
            BuiltinEffect::Mosaic { block_size } => {
                if *block_size <= 1.0 {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &MosaicParams {
                    width, height,
                    block_size: *block_size,
                    _pad: 0,
                });
                (&self.mosaic_pipeline, slot)
            }
            BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } => {
                if *amount < 0.01 {
                    return Recorded::Identity;
                }
                let s = (*samples).clamp(4, 32);
                let slot = rec.slots.claim(q, &RadialBlurParams {
                    width, height,
                    center_x: *center_x,
                    center_y: *center_y,
                    amount: *amount,
                    samples: s,
                    mode: *mode,
                    _pad: 0,
                });
                (&self.radial_blur_pipeline, slot)
            }
            BuiltinEffect::Sharpen { amount } => {
                if amount.abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                // Single-pass 3x3 unsharp mask — the shader samples neighbors directly
                let slot = rec.slots.claim(q, &SharpenParams {
                    width,
                    height,
                    amount: *amount,
                    _pad: 0,
                });
                (&self.sharpen_pipeline, slot)
            }
            BuiltinEffect::MedianFilter { radius } => {
                let r = (*radius).clamp(1, 2); // clamp to 2 on GPU for performance
                let slot = rec.slots.claim(q, &MedianFilterParams {
                    width, height,
                    radius: r,
                    _pad: 0,
                });
                (&self.median_filter_pipeline, slot)
            }
            BuiltinEffect::ChromaKey {
                key_color,
                tolerance,
                soft_edge,
                spill_method,
                spill_strength,
            } => {
                // Convert key color to CbCr
                let rf = key_color[0] as f32;
                let gf = key_color[1] as f32;
                let bf = key_color[2] as f32;
                let key_cb = 128.0 + (-0.168736 * rf - 0.331264 * gf + 0.5 * bf);
                let key_cr = 128.0 + (0.5 * rf - 0.418688 * gf - 0.081312 * bf);

                // Dominant key channel — same logic as the CPU
                // path's `dominant_channel`. Inlined here to keep
                // the GPU encoder self-contained.
                let key_idx: u32 = if key_color[1] >= key_color[0] && key_color[1] >= key_color[2] {
                    1
                } else if key_color[2] > key_color[0] {
                    2
                } else {
                    0
                };
                let method_code: u32 = match spill_method {
                    crate::SpillMethod::None => 0,
                    crate::SpillMethod::ChannelClamp => 1,
                    crate::SpillMethod::Hawk => 2,
                };

                let slot = rec.slots.claim(q, &ChromaKeyParams {
                    width,
                    height,
                    key_cb,
                    key_cr,
                    tolerance: *tolerance,
                    soft_edge: *soft_edge,
                    spill_method: method_code,
                    spill_strength: *spill_strength,
                    spill_key_idx: key_idx,
                    _pad: [0; 3],
                });
                (&self.chroma_key_pipeline, slot)
            }
            // The grade's luma key op, its matte taken into alpha (#1638).
            // Its range is what goes, so the op keeps the inverse of it.
            BuiltinEffect::LumaKey { low, high, soft, invert } => {
                let slot = rec.slots.claim(q, &super::GpuLumaKeyParams {
                    width, height,
                    low: *low, high: *high, soft: *soft,
                    invert: u32::from(!*invert),
                    into_alpha: 1,
                    _pad1: 0,
                    luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
                });
                (&self.luma_key_pipeline, slot)
            }
            BuiltinEffect::Deflicker { strength, gain, .. } => {
                // `window_secs` shaped the gains during analysis
                // and the compositor picked this frame's out of
                // them; by here the answer is one number.
                let slot = rec.slots.claim(q, &DeflickerParams {
                    width, height,
                    gain: *gain,
                    strength: strength.clamp(0.0, 1.0),
                });
                (&self.deflicker_pipeline, slot)
            }
            BuiltinEffect::FilmGrain { amount, size, color, shadows, seed } => {
                if *amount < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &FilmGrainParams {
                    width, height,
                    amount: *amount, per_px: crate::cpu::grain_cells_per_px(*size, height), color: *color, shadows: *shadows, seed: *seed,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2],
                    _pad: 0, _pad2: 0,
                });
                (&self.film_grain_pipeline, slot)
            }
            BuiltinEffect::NoiseGenerator { amount, monochrome, seed } => {
                if *amount < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &NoiseGeneratorParams {
                    width, height,
                    amount: *amount,
                    monochrome: *monochrome,
                    seed: *seed,
                    _pad: 0, _pad2: 0, _pad3: 0,
                });
                (&self.noise_generator_pipeline, slot)
            }
            BuiltinEffect::OldFilm { flicker, scratches, dust, weave, sepia, seed } => {
                let slot = rec.slots.claim(q, &OldFilmParams {
                    width, height,
                    flicker: *flicker, scratches: *scratches, dust: *dust, weave: *weave, sepia: *sepia, seed: *seed,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad: 0,
                });
                (&self.old_film_pipeline, slot)
            }
            BuiltinEffect::Vhs { tracking, bleed, noise, scanlines, seed } => {
                let slot = rec.slots.claim(q, &VhsParams {
                    width, height,
                    tracking: *tracking, bleed: *bleed, noise: *noise, scanlines: *scanlines, seed: *seed,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2],
                    _pad: 0, _pad2: 0,
                });
                (&self.vhs_pipeline, slot)
            }
            BuiltinEffect::CornerPin { tl, tr, br, bl } => {
                let corners = [*tl, *tr, *br, *bl];
                if crate::corner_pin::is_still(&corners) {
                    return Recorded::Identity;
                }
                // No quadrilateral: nothing is drawn. The shader
                // reads a zero denominator everywhere as "behind
                // the eye" and writes transparent.
                let inv = crate::corner_pin::inverse_map(width, height, &corners).unwrap_or([0.0; 9]);
                let slot = rec.slots.claim(q, &CornerPinParams { width, height, inv, _pad: 0 });
                (&self.corner_pin_pipeline, slot)
            }
            BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {
                let slot = rec.slots.claim(q, &AnamorphicStreakParams {
                    width, height,
                    threshold: *threshold,
                    half_len: (*length * 0.5) as u32,
                    intensity: *intensity,
                    tint: *tint,
                    _pad: 0, _pad2: 0,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
                });
                (&self.anamorphic_streak_pipeline, slot)
            }
            BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
                if red_shift.abs() < f32::EPSILON && blue_shift.abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &ChromaticAberrationParams {
                    width, height,
                    red_shift: *red_shift,
                    blue_shift: *blue_shift,
                    center_x: *center_x,
                    center_y: *center_y,
                    _pad0: 0,
                    _pad1: 0,
                });
                (&self.chromatic_aberration_pipeline, slot)
            }
            BuiltinEffect::LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
                let slot = rec.slots.claim(q, &LensFlareParams {
                    width, height,
                    pos_x: *position_x, pos_y: *position_y,
                    brightness: *brightness, flare_size: *flare_size,
                    ghost_count: *ghost_count, ghost_intensity: *ghost_intensity,
                    chromatic_aberration: *chromatic_aberration,
                    _pad: 0, _pad2: 0, _pad3: 0,
                });
                (&self.lens_flare_pipeline, slot)
            }
            BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
                let slot = rec.slots.claim(q, &LightRaysParams {
                    width, height,
                    center_x: *center_x,
                    center_y: *center_y,
                    threshold: *threshold,
                    samples: (*samples).clamp(4, 128),
                    decay: *decay,
                    intensity: *intensity,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
                });
                (&self.light_rays_pipeline, slot)
            }
            BuiltinEffect::Particles { .. } => {
                // The twin of `particles::apply` (#1141).
                let Some(pp) = crate::particles::params(effect, width, height) else {
                    return Recorded::Identity;
                };
                let slot = rec.slots.claim(q, &pp);
                (&self.particles_pipeline, slot)
            }
            BuiltinEffect::BinocularVignette { amount, spacing, width: eye_w, height: eye_h, softness, fringe } => {
                if amount.abs() < f32::EPSILON && fringe.abs() < f32::EPSILON {
                    return Recorded::Identity;
                }
                let slot = rec.slots.claim(q, &super::BinocularVignetteParams {
                    width, height,
                    amount: *amount, spacing: *spacing, eye_w: *eye_w, eye_h: *eye_h, softness: *softness, fringe: *fringe,
                });
                (&self.binocular_vignette_pipeline, slot)
            }
            &BuiltinEffect::Generator { shape, color_a, color_b, angle, center_x, center_y, radius } => {
                let g = crate::generator::Gen::new(shape, color_a, color_b, angle, center_x, center_y, radius, width, height);
                let slot = rec.slots.claim(q, &GeneratorParams {
                    width, height, shape: g.shape, _pad: 0,
                    a: g.a, b: g.b,
                    dir: g.dir, centre: g.centre,
                    radius: g.radius, _pad2: [0; 3],
                });
                (&self.generator_pipeline, slot)
            }
            BuiltinEffect::Vignette { amount, size, softness, roundness } => {
                let slot = rec.slots.claim(q, &VignetteParams {
                    width, height,
                    amount: *amount, size: *size, softness: *softness, roundness: *roundness,
                    _pad: 0, _pad2: 0,
                });
                (&self.vignette_pipeline, slot)
            }
            _ => return Recorded::NotRecordable,
        };
        let Some(slot) = slot else { return Recorded::OutOfSlots };
        self.record_pass(rec, pipeline, src, dst, slot, width, height);
        Recorded::Done
    }

    /// A run of DropShadow and OuterGlow effects, recorded as the one
    /// layer they make together: each spill cast from the unchanged
    /// picture, stacked under it. Not the same as recording them one by
    /// one (the second would be cast from the first's shadow), so a run
    /// of them is one call. `Identity` when none of them lays anything.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn record_spill_run(
        &self,
        effects: &[BuiltinEffect],
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        scratch: Option<&wgpu::Buffer>,
        width: u32,
        height: u32,
        rec: &mut EffectRecording<'_>,
    ) -> Recorded {
        let Some(run) = effects.iter().map(crate::layer::Spill::of).collect::<Option<Vec<_>>>() else {
            return Recorded::NotRecordable;
        };
        let Some(cast) = scratch.filter(|_| !self.is_device_lost() && self.layer_ready(width, height)) else {
            return Recorded::NotRecordable;
        };
        let needed = Self::spill_run_slots(&run, width, height);
        if needed > rec.slots.len() {
            return Recorded::NotRecordable;
        }
        if needed > rec.slots.remaining() {
            return Recorded::OutOfSlots;
        }
        if self.record_spills(rec, src, dst, cast, width, height, &run) { Recorded::Done } else { Recorded::Identity }
    }

    /// The effects of several passes, or `None` for a one-pass effect.
    /// Their passes ping-pong between `dst` and `scratch`, chosen so the
    /// last lands in `dst`; `src` is only read.
    #[allow(clippy::too_many_arguments)]
    fn record_multi_pass(
        &self,
        effect: &BuiltinEffect,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        scratch: Option<&wgpu::Buffer>,
        width: u32,
        height: u32,
        rec: &mut EffectRecording<'_>,
    ) -> Option<Recorded> {
        let q = &self.queue;
        let nonzero = |radius: f32| -> Vec<usize> { crate::blur::boxes_for_gaussian(radius).into_iter().filter(|r| *r != 0).collect() };
        let blur = |radius: usize| BlurParams { width, height, radius: radius as i32, _pad: 0 };
        let outcome = match effect {
            // SpeedRamp is the engine's (frame queue and blend), temporal
            // denoise runs at the head of the clip's pipeline before the
            // grade, and a Frei0r plug-in has no GPU surface.
            BuiltinEffect::SpeedRamp { .. } | BuiltinEffect::TemporalDenoise { .. } | BuiltinEffect::Frei0r { .. } => Recorded::Identity,
            // Drawn by the plan's layer mixer, with its foreground (#1746).
            BuiltinEffect::Merge { .. } => Recorded::Identity,
            // Read by the executor from its frame context (#1746).
            BuiltinEffect::MediaSource { .. } | BuiltinEffect::ClipBelow {} | BuiltinEffect::GradeCompound {} => Recorded::Identity,
            // The motion data is the node's analysis for the frame, uploaded
            // before it records (M4 step 9); without it the frame passes
            // through.
            BuiltinEffect::RollingShutterCorrection { strength, .. } => {
                let Some(rows) = self.rsc_row_buf.as_ref().filter(|_| *strength >= f32::EPSILON && self.has_rsc_data()) else {
                    return Some(Recorded::Identity);
                };
                let Some(slot) = rec.slots.claim(q, &RscParams { width, height, strength: *strength, _pad: 0 }) else { return Some(Recorded::OutOfSlots) };
                self.record_with(rec, &self.rsc_pipeline, src, dst, slot, rows, width, height);
                Recorded::Done
            }
            BuiltinEffect::VideoStabilization { crop_ratio, zoom, .. } => {
                let Some(homographies) = self.stabilization_buf.as_ref().filter(|_| self.has_stabilization_data()) else {
                    return Some(Recorded::Identity);
                };
                let crop_zoom = crate::stabilization_crop_zoom(*crop_ratio, *zoom);
                let Some(slot) = rec.slots.claim(q, &StabilizationParams { width, height, crop_zoom, _pad: 0 }) else { return Some(Recorded::OutOfSlots) };
                self.record_with(rec, &self.stabilization_pipeline, src, dst, slot, homographies, width, height);
                Recorded::Done
            }
            BuiltinEffect::RoundedCorners { radius, feather } => {
                let Some(bounds) = scratch.filter(|_| self.layer_ready(width, height)) else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                self.record_layer_corners(rec, src, dst, bounds, width, height, *radius, *feather);
                Recorded::Done
            }
            BuiltinEffect::FillBars { blur, zoom, darken } => {
                let Some(bounds) = scratch.filter(|_| self.layer_ready(width, height)) else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                self.record_layer_fill(rec, src, dst, bounds, width, height, *blur, *zoom, *darken);
                Recorded::Done
            }
            BuiltinEffect::DropShadow { .. } | BuiltinEffect::OuterGlow { .. } => {
                return Some(self.record_spill_run(std::slice::from_ref(effect), src, dst, scratch, width, height, rec));
            }
            BuiltinEffect::Bloom { threshold, radius, intensity } => {
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let seed = rec.slots.claim(q, &self.bloom_threshold(width, height, *threshold));
                let blend = rec.slots.claim(q, &self.bloom_blend(width, height, *intensity, 0.0));
                let light = LightLayer { seed, radius: *radius, blend: &self.bloom_blend_pipeline, blend_slot: blend };
                self.record_light_layer(rec, src, dst, scratch, light, width, height);
                Recorded::Done
            }
            BuiltinEffect::Diffusion { amount, radius, threshold, desaturation } => {
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let seed = rec.slots.claim(q, &self.bloom_threshold(width, height, *threshold));
                let blend = rec.slots.claim(q, &self.bloom_blend(width, height, *amount, *desaturation));
                let light = LightLayer { seed, radius: *radius, blend: &self.glow_blend_pipeline, blend_slot: blend };
                self.record_light_layer(rec, src, dst, scratch, light, width, height);
                Recorded::Done
            }
            BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
                // The brights, tinted by the halation's colour in the blend.
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let seed = rec.slots.claim(q, &self.bloom_threshold(width, height, *threshold));
                let blend = rec.slots.claim(q, &FilmHalationBlendParams {
                    width, height, threshold: *threshold, intensity: *intensity,
                    hal_r: 1.0, hal_g: *hue * 0.6, hal_b: *hue * *hue * 0.2, _pad: 0,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
                });
                let light = LightLayer { seed, radius: *radius, blend: &self.film_halation_blend_pipeline, blend_slot: blend };
                self.record_light_layer(rec, src, dst, scratch, light, width, height);
                Recorded::Done
            }
            BuiltinEffect::Glow { radius, intensity, threshold } => {
                // The whole picture blurred and screened over itself, its
                // light spreading past the edges of a layer with clear
                // parts, a title's (#2703).
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let seed = rec.slots.claim(q, &BloomThresholdParams { premultiply: 1, ..self.bloom_threshold(width, height, *threshold) });
                let blend = rec.slots.claim(q, &BloomBlendParams { lift_alpha: 1, ..self.bloom_blend(width, height, *intensity, 0.0) });
                let light = LightLayer { seed, radius: *radius, blend: &self.glow_blend_pipeline, blend_slot: blend };
                self.record_light_layer(rec, src, dst, scratch, light, width, height);
                Recorded::Done
            }
            BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let blend = rec.slots.claim(q, &self.bloom_blend(width, height, *intensity, 0.0));
                let light = LightLayer { seed: None, radius: *wrap_w * *softness, blend: &self.glow_blend_pipeline, blend_slot: blend };
                self.record_light_layer(rec, src, dst, scratch, light, width, height);
                Recorded::Done
            }
            BuiltinEffect::GaussianBlur { radius } => {
                // A three-box approximation, each box a horizontal and a
                // vertical pass.
                let boxes = if *radius < 0.5 { Vec::new() } else { nonzero(*radius) };
                if boxes.is_empty() {
                    return Some(Recorded::Identity);
                }
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let passes = boxes.len() * 2;
                let mut from = src;
                for (i, box_r) in boxes.into_iter().enumerate() {
                    let slot = rec.slots.claim(q, &blur(box_r)).expect("slots_needed was checked");
                    for (j, pipeline) in [&self.blur_h_pipeline, &self.blur_v_pipeline].into_iter().enumerate() {
                        let to = ends_in_dst(i * 2 + j, passes, dst, scratch);
                        self.record_pass(rec, pipeline, from, to, slot, width, height);
                        from = to;
                    }
                }
                Recorded::Done
            }
            BuiltinEffect::Denoise { spatial_sigma, color_sigma, iterations } => {
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let passes = (*iterations).clamp(1, 3) as usize;
                let slot = rec.slots.claim(q, &DenoiseParams {
                    width, height,
                    spatial_sigma: *spatial_sigma,
                    luma_sigma: *color_sigma,
                    luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2],
                    // The effect's sigma is scaled with the render
                    // size; its kernel keeps the cap it always had.
                    max_kernel: 10,
                    // The effect has one colour sigma for all of rgb,
                    // as its CPU twin in `cpu.rs` does: the same sigma
                    // for luma and chroma is that filter (#1039).
                    chroma_sigma: *color_sigma,
                    _pad: [0; 3],
                }).expect("slots_needed was checked");
                let mut from = src;
                for i in 0..passes {
                    let to = ends_in_dst(i, passes, dst, scratch);
                    self.record_pass(rec, &self.denoise_pipeline, from, to, slot, width, height);
                    from = to;
                }
                Recorded::Done
            }
            BuiltinEffect::TiltShift { center, width: band, softness, radius } => {
                // The blurred frame into `scratch`, then the band mask
                // blends the original back over it.
                let boxes = if *radius < 0.5 { Vec::new() } else { nonzero(*radius) };
                if boxes.is_empty() {
                    return Some(Recorded::Identity);
                }
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let mut from = src;
                for box_r in boxes {
                    let slot = rec.slots.claim(q, &blur(box_r)).expect("slots_needed was checked");
                    self.record_pass(rec, &self.blur_h_pipeline, from, dst, slot, width, height);
                    self.record_pass(rec, &self.blur_v_pipeline, dst, scratch, slot, width, height);
                    from = scratch;
                }
                let slot = rec.slots.claim(q, &TiltShiftParams { width, height, center: *center, band: *band, softness: *softness, _pad: 0, _pad2: 0, _pad3: 0 });
                self.record_pair(rec, &self.tilt_shift_blend_pipeline, src, scratch, dst, slot.expect("slots_needed was checked"), width, height);
                Recorded::Done
            }
            BuiltinEffect::FocusBlur { radius, vertical, band } => {
                // The layer premultiplied into `scratch` and blurred
                // there, so nothing bleeds from colour under no alpha;
                // then the band's mix of it over the original, alpha too.
                let boxes = if *radius < 0.5 || *band >= 0.999 { Vec::new() } else { nonzero(*radius) };
                if boxes.is_empty() {
                    return Some(Recorded::Identity);
                }
                let Some(scratch) = scratch else { return Some(Recorded::NotRecordable) };
                if !has_room(rec, effect) {
                    return Some(Recorded::OutOfSlots);
                }
                let slot = rec.slots.claim(q, &blur(0)).expect("slots_needed was checked");
                self.record_pass(rec, &self.focus_blur_premultiply_pipeline, src, scratch, slot, width, height);
                for box_r in boxes {
                    let slot = rec.slots.claim(q, &blur(box_r)).expect("slots_needed was checked");
                    self.record_pass(rec, &self.blur_h_pipeline, scratch, dst, slot, width, height);
                    self.record_pass(rec, &self.blur_v_pipeline, dst, scratch, slot, width, height);
                }
                let slot = rec.slots.claim(q, &FocusBlurParams { width, height, vertical: u32::from(*vertical), band: *band });
                self.record_pair(rec, &self.focus_blur_blend_pipeline, src, scratch, dst, slot.expect("slots_needed was checked"), width, height);
                Recorded::Done
            }
            _ => return None,
        };
        Some(outcome)
    }

    /// A light effect's layer: `src` into `scratch` (its brights through
    /// the threshold pass when `light.seed` names that pass's slot, else
    /// a copy), blurred there by three boxes, then blended over `src`
    /// into `dst`.
    #[allow(clippy::too_many_arguments)]
    fn record_light_layer(
        &self,
        rec: &mut EffectRecording<'_>,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        scratch: &wgpu::Buffer,
        light: LightLayer<'_>,
        width: u32,
        height: u32,
    ) {
        match light.seed {
            Some(slot) => self.record_pass(rec, &self.bloom_threshold_pipeline, src, scratch, slot, width, height),
            None => rec.encoder.copy_buffer_to_buffer(src, 0, scratch, 0, u64::from(width * height) * 16),
        }
        for box_r in crate::blur::boxes_for_gaussian(light.radius) {
            let slot = rec.slots.claim(&self.queue, &BlurParams { width, height, radius: box_r as i32, _pad: 0 });
            let slot = slot.expect("slots_needed was checked");
            self.record_pass(rec, &self.blur_h_pipeline, scratch, dst, slot, width, height);
            self.record_pass(rec, &self.blur_v_pipeline, dst, scratch, slot, width, height);
        }
        self.record_pair(rec, light.blend, src, scratch, dst, light.blend_slot.expect("slots_needed was checked"), width, height);
    }

    fn bloom_threshold(&self, width: u32, height: u32, threshold: f32) -> BloomThresholdParams {
        BloomThresholdParams {
            width, height, threshold, premultiply: 0,
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
        }
    }

    fn bloom_blend(&self, width: u32, height: u32, intensity: f32, desat: f32) -> BloomBlendParams {
        BloomBlendParams {
            width, height, intensity, desat,
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], lift_alpha: 0,
        }
    }

    /// One pass of a three-binding pipeline (input, output, params).
    #[allow(clippy::too_many_arguments)]
    fn record_pass(
        &self,
        rec: &mut EffectRecording<'_>,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        slot: usize,
        width: u32,
        height: u32,
    ) {
        self.record_pass_n(rec, pipeline, input, output, slot, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    /// [`Self::record_pass`] over `workgroups` workgroups, for a pass that
    /// isn't one lane per pixel.
    pub(super) fn record_pass_n(
        &self,
        rec: &mut EffectRecording<'_>,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        slot: usize,
        workgroups: u32,
    ) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("recorded_effect"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: rec.slots.buffer(slot).as_entire_binding() },
            ],
        });
        Self::record_compute_dispatch(rec.encoder, pipeline, &bind_group, workgroups);
    }

    /// One pass of a pipeline with a side buffer after its params
    /// (input, output, params, side).
    #[allow(clippy::too_many_arguments)]
    fn record_with(
        &self,
        rec: &mut EffectRecording<'_>,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        slot: usize,
        side: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("recorded_effect_side"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: rec.slots.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: side.as_entire_binding() },
            ],
        });
        Self::record_compute_dispatch(rec.encoder, pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    /// One pass of a pipeline that reads its slot's table after its params
    /// (input, output, params, table).
    #[allow(clippy::too_many_arguments)]
    /// `lut`'s table as a storage buffer, uploaded the first time it is
    /// asked for; the last few are kept.
    fn effect_lut_buffer(&self, lut: &Arc<crate::colour_transform::LutTable>) -> Arc<wgpu::Buffer> {
        let mut kept = self.effect_luts.borrow_mut();
        if let Some(i) = kept.iter().position(|(l, _)| Arc::ptr_eq(l, lut)) {
            let entry = kept.remove(i);
            let buffer = entry.1.clone();
            kept.push(entry);
            return buffer;
        }
        let buffer = Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("effect_lut"),
            size: (lut.data.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&lut.data));
        if kept.len() >= 4 {
            kept.remove(0);
        }
        kept.push((lut.clone(), buffer.clone()));
        buffer
    }

    fn record_with_table(
        &self,
        rec: &mut EffectRecording<'_>,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        slot: usize,
        width: u32,
        height: u32,
    ) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("recorded_effect_table"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: rec.slots.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Buffer(rec.slots.table(slot)) },
            ],
        });
        Self::record_compute_dispatch(rec.encoder, pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    /// One pass of a pipeline that reads two frames (a, b, output, params).
    #[allow(clippy::too_many_arguments)]
    fn record_pair(
        &self,
        rec: &mut EffectRecording<'_>,
        pipeline: &wgpu::ComputePipeline,
        a: &wgpu::Buffer,
        b: &wgpu::Buffer,
        output: &wgpu::Buffer,
        slot: usize,
        width: u32,
        height: u32,
    ) {
        self.record_pair_n(rec, pipeline, a, b, output, slot, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    /// [`Self::record_pair`] over `workgroups` workgroups.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_pair_n(
        &self,
        rec: &mut EffectRecording<'_>,
        pipeline: &wgpu::ComputePipeline,
        a: &wgpu::Buffer,
        b: &wgpu::Buffer,
        output: &wgpu::Buffer,
        slot: usize,
        workgroups: u32,
    ) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("recorded_effect_pair"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: rec.slots.buffer(slot).as_entire_binding() },
            ],
        });
        Self::record_compute_dispatch(rec.encoder, pipeline, &bind_group, workgroups);
    }
}

/// Whether `rec` has the slots `effect` claims at most, checked by an
/// arm of several passes before it records the first.
fn has_room(rec: &EffectRecording<'_>, effect: &BuiltinEffect) -> bool {
    rec.slots.remaining() >= slots_needed(effect).unwrap_or(0)
}

/// A light effect's layer for `record_light_layer`: the threshold pass's
/// slot (`None` copies the picture instead), the blur, and the blend.
struct LightLayer<'p> {
    seed: Option<usize>,
    radius: f32,
    blend: &'p wgpu::ComputePipeline,
    blend_slot: Option<usize>,
}

/// Where pass `i` of `passes` writes, ping-ponging between `dst` and
/// `scratch` so that the last pass writes `dst`.
fn ends_in_dst<'b>(i: usize, passes: usize, dst: &'b wgpu::Buffer, scratch: &'b wgpu::Buffer) -> &'b wgpu::Buffer {
    if (passes - 1 - i).is_multiple_of(2) { dst } else { scratch }
}
