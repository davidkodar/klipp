//! The light, glow and look effects (#1006).
//!
//! Defaults are what resetting the effect gives (the inspector's
//! `builtin_effect_default`); soft ranges are the inspector sliders'.
//! No hard ranges: none of these refuses a value. All of them act on
//! linear light.
//!
//! Lengths come in two kinds, and the units say which: glow, bloom,
//! halation and diffusion radii, the light wrap's width, the streak's
//! length and the fringe's shifts are canvas pixels, scaled with the
//! render size; a drop shadow's offsets and blur and an outer glow's
//! radius are fractions of the frame.
//!
//! Where two places disagree, the description follows the code that
//! renders and says so:
//!
//! * Light rays' sample count has a slider (4..128, the range the CPU
//!   clamps to) but is not keyframable, and lens flare's ghost count is
//!   a whole number with no keyframes.
//! * A drop shadow and an outer glow reset to nothing (zero opacity,
//!   zero intensity): adding one changes nothing until it is set.

use super::{
    mixable,
    Alpha, Category, ColourDomain, EffectType, NodeTypeDesc, ParamDesc, ParamKind, Scaling, Spatial, Temporal, Unit,
    IMAGE_IN, IMAGE_OUT,
};
use crate::BuiltinEffect;

const fn light(id: &'static str, name: &'static str, params: &'static [ParamDesc], spatial: Spatial, draws_outside: bool) -> NodeTypeDesc {
    NodeTypeDesc {
        id,
        name,
        category: Category::LightGlow,
        inputs: IMAGE_IN,
        outputs: IMAGE_OUT,
        params,
        domain: ColourDomain::Linear,
        alpha: Alpha::Straight,
        spatial,
        temporal: Temporal::None,
        draws_outside,
        gpu: true,
        cpu: true,
        mixable: false,
    }
}

/// A whole-number parameter that is not keyframed.
const fn whole(field: &'static str, label: &'static str, default: f32, soft: (f32, f32)) -> ParamDesc {
    ParamDesc {
        field,
        index: None,
        key: field,
        label,
        kind: ParamKind::Whole,
        default,
        range: None,
        soft: Some(soft),
        unit: Unit::Count,
        scaling: Scaling::None,
        keyframable: false,
        derived: false,
    }
}

/// An authored colour.
const fn colour(field: &'static str, label: &'static str) -> ParamDesc {
    ParamDesc {
        field,
        index: None,
        key: field,
        label,
        kind: ParamKind::Colour,
        default: f32::NAN,
        range: None,
        soft: None,
        unit: Unit::None,
        scaling: Scaling::None,
        keyframable: false,
        derived: false,
    }
}

pub(super) static TYPES: &[EffectType] = &[
    EffectType {
        desc: mixable(light(
            "klipp.fx.bloom",
            "Bloom",
            &[
                ParamDesc::float("threshold", "Threshold", 0.7, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("radius", "Radius", 20.0, (0.0, 100.0), Unit::CanvasPx),
                ParamDesc::float("intensity", "Intensity", 0.5, (0.0, 2.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            false,
        )),
        tag: "bloom",
        sample: || BuiltinEffect::Bloom { threshold: 0.7, radius: 20.0, intensity: 0.5 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.glow",
            "Glow",
            &[
                ParamDesc::float("radius", "Radius", 30.0, (0.0, 100.0), Unit::CanvasPx),
                ParamDesc::float("intensity", "Intensity", 0.4, (0.0, 2.0), Unit::Ratio),
                ParamDesc::float("threshold", "Threshold", 0.0, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            false,
        )),
        tag: "glow",
        sample: || BuiltinEffect::Glow { radius: 30.0, intensity: 0.4, threshold: 0.0 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.light_rays",
            "Light Rays",
            &[
                ParamDesc::float("center_x", "Center X", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("center_y", "Center Y", 0.3, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("threshold", "Threshold", 0.6, (0.0, 1.0), Unit::Ratio),
                whole("samples", "Samples", 64.0, (4.0, 128.0)),
                ParamDesc::float("decay", "Decay", 0.3, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("intensity", "Intensity", 0.5, (0.0, 2.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            false,
        )),
        tag: "light_rays",
        sample: || BuiltinEffect::LightRays { center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 64, decay: 0.3, intensity: 0.5 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.vignette",
            "Vignette",
            &[
                ParamDesc::float("amount", "Amount", -0.4, (-1.0, 1.0), Unit::Ratio),
                ParamDesc::float("size", "Size", 0.8, (0.0, 2.0), Unit::Normalised),
                ParamDesc::float("softness", "Softness", 0.5, (0.0, 2.0), Unit::Normalised),
                ParamDesc::float("roundness", "Roundness", 0.5, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::PerPixel,
            false,
        )),
        tag: "vignette",
        sample: || BuiltinEffect::Vignette { amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.5 },
    },
    // Two eyepieces (#989): their spacing a share of the width, their
    // size shares of the height; the fringe in canvas pixels.
    EffectType {
        desc: mixable(light(
            "klipp.fx.binocular_vignette",
            "Binocular Vignette",
            &[
                ParamDesc::float("amount", "Amount", -1.0, (-1.0, 1.0), Unit::Ratio),
                ParamDesc::float("spacing", "Eye Spacing", 0.4, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("width", "Width", 0.85, (0.1, 2.0), Unit::Normalised),
                ParamDesc::float("height", "Height", 0.85, (0.1, 2.0), Unit::Normalised),
                ParamDesc::float("softness", "Softness", 0.15, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("fringe", "Fringe", 4.0, (-20.0, 20.0), Unit::CanvasPx),
            ],
            Spatial::Geometric,
            false,
        )),
        tag: "binocular_vignette",
        sample: || BuiltinEffect::BinocularVignette { amount: -1.0, spacing: 0.4, width: 0.85, height: 0.85, softness: 0.15, fringe: 4.0 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.diffusion",
            "Diffusion",
            &[
                ParamDesc::float("amount", "Amount", 0.3, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("radius", "Radius", 25.0, (0.0, 100.0), Unit::CanvasPx),
                ParamDesc::float("threshold", "Threshold", 0.5, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("desaturation", "Desaturation", 0.2, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            false,
        )),
        tag: "diffusion",
        sample: || BuiltinEffect::Diffusion { amount: 0.3, radius: 25.0, threshold: 0.5, desaturation: 0.2 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.lens_flare",
            "Lens Flare",
            &[
                ParamDesc::float("position_x", "Position X", 0.7, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("position_y", "Position Y", 0.3, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("brightness", "Brightness", 0.6, (0.0, 2.0), Unit::Ratio),
                ParamDesc::float("flare_size", "Flare Size", 0.15, (0.0, 1.0), Unit::Normalised),
                whole("ghost_count", "Ghost Count", 4.0, (0.0, 8.0)),
                ParamDesc::float("ghost_intensity", "Ghost Intensity", 0.3, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("chromatic_aberration", "Chromatic Aberration", 0.4, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::PerPixel,
            false,
        )),
        tag: "lens_flare",
        sample: || BuiltinEffect::LensFlare {
            position_x: 0.7,
            position_y: 0.3,
            brightness: 0.6,
            flare_size: 0.15,
            ghost_count: 4,
            ghost_intensity: 0.3,
            chromatic_aberration: 0.4,
        },
    },
    EffectType {
        desc: light(
            "klipp.fx.outer_glow",
            "Outer Glow",
            &[
                ParamDesc::float("radius", "Radius", 0.0, (0.0, 0.3), Unit::Normalised),
                ParamDesc::float("intensity", "Intensity", 0.0, (0.0, 1.0), Unit::Ratio),
                colour("color", "Colour"),
            ],
            Spatial::Neighbourhood,
            true,
        ),
        tag: "outer_glow",
        sample: || BuiltinEffect::OuterGlow { radius: 0.0, intensity: 0.0, color: [255, 255, 255] },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.light_wrap",
            "Light Wrap",
            &[
                ParamDesc::float("width", "Width", 15.0, (0.0, 100.0), Unit::CanvasPx),
                ParamDesc::float("intensity", "Intensity", 0.5, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("softness", "Softness", 1.0, (0.0, 2.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            false,
        )),
        tag: "light_wrap",
        sample: || BuiltinEffect::LightWrap { width: 15.0, intensity: 0.5, softness: 1.0 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.anamorphic_streak",
            "Anamorphic Streak",
            &[
                ParamDesc::float("threshold", "Threshold", 0.7, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("length", "Length", 80.0, (0.0, 200.0), Unit::CanvasPx),
                ParamDesc::float("intensity", "Intensity", 0.5, (0.0, 2.0), Unit::Ratio),
                ParamDesc::float("tint", "Tint", 0.3, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            false,
        )),
        tag: "anamorphic_streak",
        sample: || BuiltinEffect::AnamorphicStreak { threshold: 0.7, length: 80.0, intensity: 0.5, tint: 0.3 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.film_halation",
            "Film Halation",
            &[
                ParamDesc::float("threshold", "Threshold", 0.75, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("radius", "Radius", 15.0, (0.0, 50.0), Unit::CanvasPx),
                ParamDesc::float("intensity", "Intensity", 0.3, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("hue", "Hue", 0.2, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            false,
        )),
        tag: "film_halation",
        sample: || BuiltinEffect::FilmHalation { threshold: 0.75, radius: 15.0, intensity: 0.3, hue: 0.2 },
    },
    EffectType {
        desc: mixable(light(
            "klipp.fx.chromatic_aberration",
            "Chromatic Aberration",
            &[
                ParamDesc::float("red_shift", "Red Shift", 2.0, (-20.0, 20.0), Unit::CanvasPx),
                ParamDesc::float("blue_shift", "Blue Shift", -2.0, (-20.0, 20.0), Unit::CanvasPx),
                ParamDesc::float("center_x", "Center X", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("center_y", "Center Y", 0.5, (0.0, 1.0), Unit::Normalised),
            ],
            Spatial::Geometric,
            false,
        )),
        tag: "chromatic_aberration",
        sample: || BuiltinEffect::ChromaticAberration { red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5 },
    },
    EffectType {
        desc: light(
            "klipp.fx.drop_shadow",
            "Drop Shadow",
            &[
                ParamDesc::float("offset_x", "Right", 0.0, (-0.2, 0.2), Unit::Normalised),
                ParamDesc::float("offset_y", "Down", 0.0, (-0.2, 0.2), Unit::Normalised),
                ParamDesc::float("blur", "Blur", 0.0, (0.0, 0.2), Unit::Normalised),
                colour("color", "Colour"),
                ParamDesc::float("opacity", "Opacity", 0.0, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::Neighbourhood,
            true,
        ),
        tag: "drop_shadow",
        sample: || BuiltinEffect::DropShadow { offset_x: 0.0, offset_y: 0.0, blur: 0.0, color: [0, 0, 0], opacity: 0.0 },
    },
];
