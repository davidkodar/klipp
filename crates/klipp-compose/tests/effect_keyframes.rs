//! Every parameter the inspector offers keyframes on reaches the
//! render (#992). The render read keyframes on 11 of the effects and the
//! inspector offered them on nearly all, so a keyframed Bloom, or a
//! hyperzoom's blur, fringing and halation, rendered its static value.

use klipp_compose::render::{clip_effects, temporal_denoise_of};
use klipp_core::{Interpolation, Keyframe, RationalTime, Timecode};
use klipp_effects::{BuiltinEffect as E, ColorFadeMode, SpillMethod};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::Clip;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// One of each effect, set so it does something (an identity effect is
/// dropped from the chain, and would hide whether its keyframe landed).
fn every_effect() -> Vec<E> {
    let all = vec![
        E::Crop { left: 0.1, right: 0.1, top: 0.1, bottom: 0.1, feather: 0.1 },
        E::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        },
        E::Opacity { value: 0.3 },
        E::ColorFade { color: [0, 0, 0, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress: 0.0 },
        E::GaussianBlur { radius: 3.0 },
        E::FocusBlur { radius: 3.0, vertical: false, band: 0.2 },
        E::Sharpen { amount: 0.5 },
        E::ChromaKey { key_color: [0, 255, 0], tolerance: 0.3, soft_edge: 0.1, spill_method: SpillMethod::None, spill_strength: 0.5 },
        E::LumaKey { low: 0.0, high: 0.1, soft: 0.05, invert: false },
        E::Bloom { threshold: 0.7, radius: 6.0, intensity: 0.8 },
        E::Glow { radius: 6.0, intensity: 0.5, threshold: 0.0 },
        E::LightRays { center_x: 0.5, center_y: 0.5, threshold: 0.6, samples: 16, decay: 0.9, intensity: 0.5 },
        E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 },
        E::Diffusion { amount: 0.5, radius: 4.0, threshold: 0.5, desaturation: 0.2 },
        E::LensFlare {
            position_x: 0.3, position_y: 0.3, brightness: 0.8, flare_size: 0.3, ghost_count: 3, ghost_intensity: 0.5,
            chromatic_aberration: 0.3,
        },
        E::RoundedCorners { radius: 20.0, feather: 1.0 },
        E::DropShadow { offset_x: 5.0, offset_y: 5.0, blur: 4.0, color: [0, 0, 0], opacity: 0.5 },
        E::FillBars { blur: 0.1, zoom: 1.2, darken: 0.3 },
        E::OuterGlow { radius: 6.0, intensity: 0.5, color: [255, 255, 255] },
        E::LightWrap { width: 10.0, intensity: 0.5, softness: 0.5 },
        E::AnamorphicStreak { threshold: 0.7, length: 40.0, intensity: 0.5, tint: 0.5 },
        E::FilmHalation { threshold: 0.7, radius: 8.0, intensity: 0.5, hue: 0.0 },
        E::Invert { mix: 0.5 },
        E::Posterize { levels: 4.0 },
        E::Mosaic { block_size: 8.0 },
        E::EdgeDetect { strength: 0.5, mix: 0.5 },
        E::ChannelMixer {
            red_red: 0.9, red_green: 0.1, red_blue: 0.1, green_red: 0.1, green_green: 0.9, green_blue: 0.1, blue_red: 0.1,
            blue_green: 0.1, blue_blue: 0.9,
        },
        E::DirectionalBlur { angle: 30.0, distance: 10.0, samples: 8 },
        E::Emboss { angle: 45.0, strength: 0.5, mix: 0.5 },
        E::Mirror { horizontal: 1, vertical: 0 },
        E::LensDistortion { distortion: 0.2, scale: 1.0 },
        E::Displace { amount: 6.0, size: 24.0, speed: 1.0, direction: 0, seed: 3, time: 0.4 },
        E::Reflection { waterline: 0.55, gap: 0.0, ripple_amount: 3.0, ripple_size: 12.0, ripple_speed: 1.0, breakup: 0.5, smear: 0.3, fade: 0.4, opacity: 0.8, darken: 0.0, contact: 0.0, blend: 0, tint: [20, 40, 60], tint_amount: 0.2, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: true, ripple_from: 0, seed: 3, time: 0.4 },
        E::Dilate { radius: 2 },
        E::Erode { radius: 2 },
        E::Denoise { spatial_sigma: 5.0, color_sigma: 0.1, iterations: 1 },
        E::Gamma { gamma: 1.2 },
        E::HSVAdjust { hue_shift: 10.0, saturation: 1.2, value: 1.1 },
        E::Vibrance { amount: 0.3 },
        E::NoiseGenerator { amount: 0.2, monochrome: 0, seed: 1 },
        E::MedianFilter { radius: 2 },
        E::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 0.3, samples: 16, mode: 0 },
        E::Levels { in_black: 0.1, in_white: 0.9, gamma: 1.1, out_black: 0.05, out_white: 0.95 },
        E::Equalize { strength: 0.5 },
        E::Letterbox { aspect_ratio: 2.39, bar_color: 0, opacity: 1.0 },
        E::ChromaticAberration { red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5 },
        E::FilmGrain { amount: 0.3, size: 1.0, color: 0, shadows: 0.5, seed: 0 },
        E::TiltShift { center: 0.5, width: 0.2, softness: 0.3, radius: 6.0 },
        E::OldFilm { flicker: 0.3, scratches: 0.3, dust: 0.3, weave: 0.3, sepia: 0.3, seed: 0 },
        E::Vhs { tracking: 0.3, bleed: 0.3, noise: 0.3, scanlines: 0.3, seed: 0 },
        E::RollingShutterCorrection { strength: 0.5, readout_ratio: 0.5 },
        E::TemporalDenoise { strength: 0.5, threshold: 0.02 },
        E::Deflicker { strength: 0.5, window_secs: 1.0, gain: 1.0 },
        E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true },
        E::SpeedRamp { speed: 1.0, blend_mode: 0 },
        E::Frei0r { type_id: "test".into(), params: Vec::new() },
        E::CornerPin { tl: [0.1, 0.1], tr: [0.9, 0.0], br: [1.0, 1.0], bl: [0.0, 0.9] },
        E::Lut3D { path: "look.cube".into(), opacity: 0.5, tetrahedral: true, prepared: None },
        E::ColourSpaceTransform { input_gamut: 5, input_curve: 2, output_gamut: 0, output_curve: 0, prepared: None },
        E::PictureTransform { position_x: 4.0, position_y: 2.0, scale_x: 0.9, scale_y: 0.9, rotation: 5.0, anchor_x: 0.5, anchor_y: 0.5 },
        E::Flicker { amount: 0.5, speed: 8.0, smoothness: 0.2, hold: 0.3, seed: 1, target: 0, time: 0.0 },
        E::BinocularVignette { amount: -0.8, spacing: 0.4, width: 0.8, height: 0.8, softness: 0.2, fringe: 3.0 },
        E::Particles { style: 1, amount: 200.0, size: 3.0, speed: 60.0, direction: 90.0, spread: 0.4, lifetime: 5.0, opacity: 0.8, color: [255, 255, 255], seed: 2, time: 0.0 },
        E::Merge { blend_mode: klipp_effects::BlendMode::Screen, opacity: 0.5 },
        E::Generator { shape: 1, color_a: [0, 0, 0, 255], color_b: [255, 255, 255, 255], angle: 30.0, center_x: 0.5, center_y: 0.5, radius: 0.5 },
        E::MediaSource { media: String::new(), offset: 0.0 },
        E::ClipBelow {},
        E::GradeCompound {},
    ];
    // Adding an effect stops this compiling until it is listed above.
    for e in &all {
        match e {
            E::Crop { .. } | E::Transform { .. } | E::Opacity { .. } | E::ColorFade { .. } | E::GaussianBlur { .. }
            | E::FocusBlur { .. } | E::Sharpen { .. } | E::ChromaKey { .. } | E::LumaKey { .. } | E::Bloom { .. } | E::Glow { .. }
            | E::LightRays { .. } | E::Vignette { .. } | E::Diffusion { .. } | E::LensFlare { .. }
            | E::RoundedCorners { .. } | E::DropShadow { .. } | E::FillBars { .. } | E::OuterGlow { .. }
            | E::LightWrap { .. } | E::AnamorphicStreak { .. } | E::FilmHalation { .. } | E::Invert { .. }
            | E::Posterize { .. } | E::Mosaic { .. } | E::EdgeDetect { .. } | E::ChannelMixer { .. }
            | E::DirectionalBlur { .. } | E::Emboss { .. } | E::Mirror { .. } | E::LensDistortion { .. } | E::Displace { .. } | E::Reflection { .. }
            | E::Dilate { .. } | E::Erode { .. } | E::Denoise { .. } | E::Gamma { .. } | E::HSVAdjust { .. }
            | E::Vibrance { .. } | E::NoiseGenerator { .. } | E::MedianFilter { .. } | E::RadialBlur { .. }
            | E::Levels { .. } | E::Equalize { .. } | E::Letterbox { .. } | E::ChromaticAberration { .. }
            | E::FilmGrain { .. } | E::TiltShift { .. } | E::OldFilm { .. } | E::Vhs { .. }
            | E::RollingShutterCorrection { .. } | E::TemporalDenoise { .. } | E::Deflicker { .. }
            | E::VideoStabilization { .. } | E::SpeedRamp { .. } | E::Frei0r { .. } | E::CornerPin { .. }
            | E::Lut3D { .. } | E::ColourSpaceTransform { .. } | E::PictureTransform { .. } | E::Flicker { .. } | E::BinocularVignette { .. } | E::Particles { .. } | E::Merge { .. } | E::Generator { .. } | E::MediaSource { .. } | E::ClipBelow {} | E::GradeCompound {} => {}
        }
    }
    all
}

/// The effect's variant name, for the failure message.
fn kind(e: &E) -> String {
    format!("{e:?}").split([' ', '{']).next().unwrap_or_default().to_string()
}

fn clip_with(effect: E) -> Clip {
    Clip {
        picture: klipp_timeline::ClipPictureParts { builtin_effects: vec![effect], ..Default::default() }.into(),
        ..Clip::gap(tc(0.0), tc(4.0))
    }
}

/// What the render takes from the clip at `t`: the effect chain and
/// opacity, and the temporal denoise settings, which run apart from it.
fn rendered(clip: &Clip, t: f64) -> String {
    let (fx, opacity) = clip_effects(clip, tc(t), 24.0, None, None);
    format!("{fx:?} {opacity} {:?}", temporal_denoise_of(clip, tc(t), 24.0))
}

#[test]
fn every_keyframable_parameter_reaches_the_render() {
    let mut ignored = Vec::new();
    for effect in every_effect() {
        for name in effect.animatable_params() {
            let still = clip_with(effect.clone());
            let mut keyed = still.clone();
            // A key well away from every still value above; an opacity
            // is a weight, kept inside 0..1.
            let value = if matches!(effect, E::Opacity { .. }) { 0.6 } else { 5.6 };
            keyed.edit_effect_keyframes(|v| v.insert(format!("0:{name}"), vec![key(0.0, value)]));
            if rendered(&keyed, 1.0) == rendered(&still, 1.0) {
                ignored.push(format!("{} {name}", kind(&effect)));
            }
        }
    }
    assert!(ignored.is_empty(), "keyframes the render ignores: {ignored:?}");
}

/// The hyperzoom's ramps (#992): its blur, fringing and halation
/// change between two keyed times instead of holding the still value.
#[test]
fn a_keyframed_effect_ramps_between_its_keys() {
    for (effect, name) in [
        (E::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 0.3, samples: 16, mode: 0 }, "amount"),
        (E::ChromaticAberration { red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5 }, "red_shift"),
        (E::FilmHalation { threshold: 0.7, radius: 8.0, intensity: 0.5, hue: 0.0 }, "intensity"),
        (E::Bloom { threshold: 0.7, radius: 6.0, intensity: 0.8 }, "radius"),
    ] {
        let clip = TestClip::of(clip_with(effect.clone())).key(0, name, 0.0, 1.0).key(0, name, 2.0, 9.0).build();
        let at = |t| rendered(&clip, t);
        assert_ne!(at(0.0), at(1.0), "{} {name} did not move", kind(&effect));
        assert_ne!(at(1.0), at(2.0), "{} {name} did not move", kind(&effect));
    }
}

/// Particles move by the clip's own time (#1141), wherever the clip sits.
#[test]
fn particles_are_given_the_clips_time() {
    let particles = E::Particles { style: 0, amount: 100.0, size: 2.0, speed: 30.0, direction: 0.0, spread: 0.5, lifetime: 4.0, opacity: 0.6, color: [255, 255, 255], seed: 1, time: 0.0 };
    let mut clip = clip_with(particles);
    clip.timeline_in = tc(2.0);
    clip.timeline_out = tc(6.0);
    let time_at = |t: f64| match clip_effects(&clip, tc(t), 24.0, None, None).0.first() {
        Some(E::Particles { time, .. }) => *time,
        other => panic!("{other:?}"),
    };
    assert!((time_at(2.5) - 0.5).abs() < 1e-4 && (time_at(4.0) - 2.0).abs() < 1e-4, "{} {}", time_at(2.5), time_at(4.0));
}

/// A flicker jumps by the clip's own time (#989): the render gives it the
/// time into the clip, wherever the clip sits.
#[test]
fn a_flicker_is_given_the_clips_time() {
    let flicker = E::Flicker { amount: 0.5, speed: 8.0, smoothness: 0.0, hold: 0.0, seed: 1, target: 0, time: 0.0 };
    let mut clip = clip_with(flicker);
    clip.timeline_in = tc(2.0);
    clip.timeline_out = tc(6.0);
    let time_at = |t: f64| match clip_effects(&clip, tc(t), 24.0, None, None).0.first() {
        Some(E::Flicker { time, .. }) => *time,
        other => panic!("{other:?}"),
    };
    assert!((time_at(2.5) - 0.5).abs() < 1e-4 && (time_at(3.25) - 1.25).abs() < 1e-4, "{} {}", time_at(2.5), time_at(3.25));
}

/// A key addresses the effect at its own place in the list, not the
/// first effect with a parameter of that name.
#[test]
fn keys_address_their_own_effect() {
    let clip = Clip {
        picture: klipp_timeline::ClipPictureParts {
            builtin_effects: vec![E::Bloom { threshold: 0.7, radius: 6.0, intensity: 0.8 }, E::Vibrance { amount: 0.3 }],
            ..Default::default()
        }
        .into(),
        ..Clip::gap(tc(0.0), tc(4.0))
    };
    let clip = TestClip::of(clip).key(1, "amount", 0.0, 0.9).build();
    let (fx, _) = clip_effects(&clip, tc(1.0), 24.0, None, None);
    assert!(matches!(fx[0], E::Bloom { radius, .. } if radius == 6.0), "{fx:?}");
    assert!(matches!(fx[1], E::Vibrance { amount } if (amount - 0.9).abs() < 1e-6), "{fx:?}");
}
