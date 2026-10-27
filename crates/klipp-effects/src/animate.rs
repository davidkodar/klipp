//! Which parameters of a built-in effect can be keyframed, and under
//! what names. The render animates exactly these and the inspector
//! offers keyframes on exactly these, so the two cannot drift apart: an
//! inspector diamond on a parameter the render never read is how most
//! effect keyframes came to do nothing (#992).

use crate::BuiltinEffect;

/// The callback [`BuiltinEffect::animate`] hands each parameter to: its
/// keyframe name and current value in, the value to use out.
pub type ParamFn<'a> = dyn FnMut(&'static str, f32) -> f32 + 'a;

fn real(f: &mut ParamFn, name: &'static str, v: &mut f32) {
    *v = f(name, *v);
}

/// A whole-number parameter (a sample or pass count): keyed as a float,
/// rounded back, and never below one.
fn whole(f: &mut ParamFn, name: &'static str, v: &mut u32) {
    *v = f(name, *v as f32).round().max(1.0) as u32;
}

impl BuiltinEffect {
    /// Hand every keyframable parameter of this effect to `f`, by its
    /// keyframe name, and set it to what `f` returns. A revision-1 list
    /// keys it as `"{index}:{name}"`; an effect node holds it in its
    /// `anim`, under the parameter it drives, which is the same name but
    /// for the lift's aliases (ChromaKey's `spill` drives `soft_edge`).
    ///
    /// Not here, and so never keyframable: the transform (its own
    /// keyframe tracks), colours, modes and switches, the plug-in
    /// parameters, a speed ramp (it acts on time; its keyframes would be
    /// a speed curve, which is the clip's speed keyframes), the settings
    /// that shape a whole-clip analysis, and the effects whose parameters
    /// are whole pixel radii, and a media source's offset.
    pub fn animate(&mut self, f: &mut ParamFn) {
        use BuiltinEffect as E;
        match self {
            E::Crop { left, right, top, bottom, feather, aspect, focus_x, focus_y } => {
                real(f, "left", left);
                real(f, "right", right);
                real(f, "top", top);
                real(f, "bottom", bottom);
                real(f, "feather", feather);
                real(f, "aspect", aspect);
                real(f, "focus_x", focus_x);
                real(f, "focus_y", focus_y);
            }
            E::Opacity { value } => real(f, "value", value),
            E::GaussianBlur { radius } => real(f, "radius", radius),
            E::Sharpen { amount } => real(f, "amount", amount),
            E::ChromaKey { tolerance, soft_edge, spill_strength, .. } => {
                real(f, "tolerance", tolerance);
                // The "spill" key has always addressed the soft edge.
                real(f, "spill", soft_edge);
                real(f, "spill_strength", spill_strength);
            }
            // The range and its softness; the invert is a switch (#1638).
            E::LumaKey { low, high, soft, .. } => {
                real(f, "low", low);
                real(f, "high", high);
                real(f, "soft", soft);
            }
            E::Bloom { threshold, radius, intensity } => {
                real(f, "threshold", threshold);
                real(f, "radius", radius);
                real(f, "intensity", intensity);
            }
            E::Glow { radius, intensity, threshold } => {
                real(f, "radius", radius);
                real(f, "intensity", intensity);
                real(f, "threshold", threshold);
            }
            E::LightRays { center_x, center_y, threshold, decay, intensity, .. } => {
                real(f, "center_x", center_x);
                real(f, "center_y", center_y);
                real(f, "threshold", threshold);
                real(f, "decay", decay);
                real(f, "intensity", intensity);
            }
            E::Vignette { amount, size, softness, roundness } => {
                real(f, "amount", amount);
                real(f, "size", size);
                real(f, "softness", softness);
                real(f, "roundness", roundness);
            }
            E::Diffusion { amount, radius, threshold, desaturation } => {
                real(f, "amount", amount);
                real(f, "radius", radius);
                real(f, "threshold", threshold);
                real(f, "desaturation", desaturation);
            }
            E::LensFlare { position_x, position_y, brightness, flare_size, ghost_intensity, chromatic_aberration, .. } => {
                real(f, "position_x", position_x);
                real(f, "position_y", position_y);
                real(f, "brightness", brightness);
                real(f, "flare_size", flare_size);
                real(f, "ghost_intensity", ghost_intensity);
                real(f, "chromatic_aberration", chromatic_aberration);
            }
            E::RoundedCorners { radius, feather } => {
                real(f, "radius", radius);
                real(f, "feather", feather);
            }
            E::DropShadow { offset_x, offset_y, blur, opacity, .. } => {
                real(f, "offset_x", offset_x);
                real(f, "offset_y", offset_y);
                real(f, "blur", blur);
                real(f, "opacity", opacity);
            }
            E::FillBars { blur, zoom, darken } => {
                real(f, "blur", blur);
                real(f, "zoom", zoom);
                real(f, "darken", darken);
            }
            E::OuterGlow { radius, intensity, .. } => {
                real(f, "radius", radius);
                real(f, "intensity", intensity);
            }
            E::LightWrap { width, intensity, softness } => {
                real(f, "width", width);
                real(f, "intensity", intensity);
                real(f, "softness", softness);
            }
            E::AnamorphicStreak { threshold, length, intensity, tint } => {
                real(f, "threshold", threshold);
                real(f, "length", length);
                real(f, "intensity", intensity);
                real(f, "tint", tint);
            }
            E::FilmHalation { threshold, radius, intensity, hue } => {
                real(f, "threshold", threshold);
                real(f, "radius", radius);
                real(f, "intensity", intensity);
                real(f, "hue", hue);
            }
            E::Invert { mix } => real(f, "mix", mix),
            E::Posterize { levels } => real(f, "levels", levels),
            E::Mosaic { block_size } => real(f, "block_size", block_size),
            E::EdgeDetect { strength, mix } => {
                real(f, "strength", strength);
                real(f, "mix", mix);
            }
            E::ChannelMixer { red_red, red_green, red_blue, green_red, green_green, green_blue, blue_red, blue_green, blue_blue } => {
                real(f, "red_red", red_red);
                real(f, "red_green", red_green);
                real(f, "red_blue", red_blue);
                real(f, "green_red", green_red);
                real(f, "green_green", green_green);
                real(f, "green_blue", green_blue);
                real(f, "blue_red", blue_red);
                real(f, "blue_green", blue_green);
                real(f, "blue_blue", blue_blue);
            }
            E::DirectionalBlur { angle, distance, samples } => {
                real(f, "angle", angle);
                real(f, "distance", distance);
                whole(f, "samples", samples);
            }
            E::Emboss { angle, strength, mix } => {
                real(f, "angle", angle);
                real(f, "strength", strength);
                real(f, "mix", mix);
            }
            E::LensDistortion { distortion, scale } => {
                real(f, "distortion", distortion);
                real(f, "scale", scale);
            }
            E::Displace { amount, size, speed, .. } => {
                real(f, "amount", amount);
                real(f, "size", size);
                real(f, "speed", speed);
            }
            E::Reflection { waterline, gap, ripple_amount, ripple_size, ripple_speed, breakup, smear, fade, opacity, darken, contact, tint_amount, submerge, .. } => {
                real(f, "darken", darken);
                real(f, "contact", contact);
                real(f, "waterline", waterline);
                real(f, "gap", gap);
                real(f, "ripple_amount", ripple_amount);
                real(f, "ripple_size", ripple_size);
                real(f, "ripple_speed", ripple_speed);
                real(f, "breakup", breakup);
                real(f, "smear", smear);
                real(f, "fade", fade);
                real(f, "opacity", opacity);
                real(f, "tint_amount", tint_amount);
                real(f, "submerge", submerge);
            }
            E::Denoise { spatial_sigma, color_sigma, iterations } => {
                real(f, "spatial_sigma", spatial_sigma);
                real(f, "color_sigma", color_sigma);
                whole(f, "iterations", iterations);
            }
            E::Gamma { gamma } => real(f, "gamma", gamma),
            E::HSVAdjust { hue_shift, saturation, value } => {
                real(f, "hue_shift", hue_shift);
                real(f, "saturation", saturation);
                real(f, "value", value);
            }
            E::Vibrance { amount } => real(f, "amount", amount),
            E::NoiseGenerator { amount, .. } => real(f, "amount", amount),
            E::RadialBlur { center_x, center_y, amount, .. } => {
                real(f, "center_x", center_x);
                real(f, "center_y", center_y);
                real(f, "amount", amount);
            }
            E::Levels { in_black, in_white, gamma, out_black, out_white } => {
                real(f, "in_black", in_black);
                real(f, "in_white", in_white);
                real(f, "gamma", gamma);
                real(f, "out_black", out_black);
                real(f, "out_white", out_white);
            }
            E::Equalize { strength } => real(f, "strength", strength),
            E::Letterbox { aspect_ratio, opacity, .. } => {
                real(f, "aspect_ratio", aspect_ratio);
                real(f, "opacity", opacity);
            }
            E::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
                real(f, "red_shift", red_shift);
                real(f, "blue_shift", blue_shift);
                real(f, "center_x", center_x);
                real(f, "center_y", center_y);
            }
            E::FilmGrain { amount, size, shadows, .. } => {
                real(f, "amount", amount);
                real(f, "size", size);
                real(f, "shadows", shadows);
            }
            E::TiltShift { center, width, softness, radius } => {
                real(f, "center", center);
                real(f, "width", width);
                real(f, "softness", softness);
                real(f, "radius", radius);
            }
            E::OldFilm { flicker, scratches, dust, weave, sepia, amount, age, mono, stains, frame_lines, leaks, burn, .. } => {
                real(f, "flicker", flicker);
                real(f, "scratches", scratches);
                real(f, "dust", dust);
                real(f, "weave", weave);
                real(f, "sepia", sepia);
                real(f, "amount", amount);
                real(f, "age", age);
                real(f, "mono", mono);
                real(f, "stains", stains);
                real(f, "frame_lines", frame_lines);
                real(f, "leaks", leaks);
                real(f, "burn", burn);
            }
            E::Vhs { tracking, bleed, noise, scanlines, .. } => {
                real(f, "tracking", tracking);
                real(f, "bleed", bleed);
                real(f, "noise", noise);
                real(f, "scanlines", scanlines);
            }
            E::RollingShutterCorrection { strength, readout_ratio } => {
                real(f, "strength", strength);
                real(f, "readout_ratio", readout_ratio);
            }
            E::TemporalDenoise { strength, threshold } => {
                real(f, "strength", strength);
                real(f, "threshold", threshold);
            }
            // The deflicker window and the stabiliser's smoothing shape
            // the whole clip's analysis, not a frame, so they hold still.
            E::Deflicker { strength, .. } => real(f, "strength", strength),
            E::VideoStabilization { crop_ratio, .. } => real(f, "crop_ratio", crop_ratio),
            E::Lut3D { opacity, .. } => real(f, "opacity", opacity),
            E::PictureTransform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y } => {
                real(f, "position_x", position_x);
                real(f, "position_y", position_y);
                real(f, "scale_x", scale_x);
                real(f, "scale_y", scale_y);
                real(f, "rotation", rotation);
                real(f, "anchor_x", anchor_x);
                real(f, "anchor_y", anchor_y);
            }
            E::Flicker { amount, speed, smoothness, hold, .. } => {
                real(f, "amount", amount);
                real(f, "speed", speed);
                real(f, "smoothness", smoothness);
                real(f, "hold", hold);
            }
            E::Particles { amount, size, speed, direction, spread, lifetime, opacity, .. } => {
                real(f, "amount", amount);
                real(f, "size", size);
                real(f, "speed", speed);
                real(f, "direction", direction);
                real(f, "spread", spread);
                real(f, "lifetime", lifetime);
                real(f, "opacity", opacity);
            }
            E::BinocularVignette { amount, spacing, width, height, softness, fringe } => {
                real(f, "amount", amount);
                real(f, "spacing", spacing);
                real(f, "width", width);
                real(f, "height", height);
                real(f, "softness", softness);
                real(f, "fringe", fringe);
            }
            E::Merge { opacity, .. } => real(f, "opacity", opacity),
            E::Generator { angle, center_x, center_y, radius, .. } => {
                real(f, "angle", angle);
                real(f, "center_x", center_x);
                real(f, "center_y", center_y);
                real(f, "radius", radius);
            }
            E::CornerPin { tl, tr, br, bl } => {
                for (corner, names) in [tl, tr, br, bl].into_iter().zip(crate::corner_pin::PARAMS) {
                    real(f, names[0], &mut corner[0]);
                    real(f, names[1], &mut corner[1]);
                }
            }
            E::SurfacePin { inset, .. } => real(f, "inset", inset),
            E::Warp { amplitude, wavelength, speed, angle, strength, radius, center_x, center_y, band, .. } => {
                real(f, "amplitude", amplitude);
                real(f, "wavelength", wavelength);
                real(f, "speed", speed);
                real(f, "angle", angle);
                real(f, "strength", strength);
                real(f, "radius", radius);
                real(f, "center_x", center_x);
                real(f, "center_y", center_y);
                real(f, "band", band);
            }
            E::Transform { .. }
            | E::ColorFade { .. }
            | E::FocusBlur { .. }
            | E::Mirror { .. }
            | E::Dilate { .. }
            | E::Erode { .. }
            | E::MedianFilter { .. }
            | E::SpeedRamp { .. }
            | E::ColourSpaceTransform { .. }
            // Its offset is a time; keyed, it would be a speed curve.
            | E::MediaSource { .. }
            | E::TitleSource { .. }
            | E::ClipBelow {}
            | E::GradeCompound {}
            | E::Frei0r { .. } => {}
        }
    }

    /// The keyframe names of this effect's keyframable parameters, in
    /// the order [`animate`](Self::animate) visits them.
    pub fn animatable_params(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        self.clone().animate(&mut |name, v| {
            names.push(name);
            v
        });
        names
    }
}
