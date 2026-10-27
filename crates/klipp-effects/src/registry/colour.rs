//! The colour and tone effects (#1004).
//!
//! Defaults are what resetting the effect gives (the inspector's
//! `builtin_effect_default`); soft ranges are the inspector sliders'.
//! No hard ranges: none of these refuses a value, and each computation
//! copes with any number it is given.
//!
//! Where two places disagree, the description follows the reset and
//! says so:
//!
//! * Vibrance resets to 0 (no change), but its slider's double-click
//!   reset is 0.5.

use super::{
    mixable,
    Alpha, Category, ColourDomain, EffectType, NodeTypeDesc, ParamDesc, ParamKind, Scaling, Spatial, Temporal, Unit,
    IMAGE_IN, IMAGE_OUT,
};
use crate::colour_transform::{CURVES, GAMUTS};
use crate::{BuiltinEffect, ColorFadeMode};

const fn colour(
    id: &'static str,
    name: &'static str,
    params: &'static [ParamDesc],
    domain: ColourDomain,
    spatial: Spatial,
) -> NodeTypeDesc {
    NodeTypeDesc {
        id,
        name,
        category: Category::Colour,
        inputs: IMAGE_IN,
        outputs: IMAGE_OUT,
        params,
        domain,
        alpha: Alpha::Straight,
        spatial,
        temporal: Temporal::None,
        draws_outside: false,
        gpu: true,
        cpu: true,
        mixable: false,
    }
}

const MIXER: (f32, f32) = (-1.0, 2.0);

pub(super) static TYPES: &[EffectType] = &[
    EffectType {
        desc: colour(
            "klipp.fx.opacity",
            "Opacity",
            &[ParamDesc::float("value", "Opacity", 1.0, (0.0, 1.0), Unit::Ratio)],
            ColourDomain::Linear,
            Spatial::PerPixel,
        ),
        tag: "opacity",
        sample: || BuiltinEffect::Opacity { value: 1.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.color_fade",
            "Colour Fade",
            &[
                ParamDesc {
                    field: "color",
                    index: None,
                    key: "color",
                    label: "Colour",
                    kind: ParamKind::Colour,
                    default: f32::NAN,
                    range: None,
                    soft: None,
                    unit: Unit::None,
                    scaling: Scaling::None,
                    keyframable: false,
                    derived: false,
                },
                ParamDesc::float("duration_secs", "Duration", 1.0, (0.05, 10.0), Unit::Seconds).fixed(),
                ParamDesc {
                    field: "mode",
                    index: None,
                    key: "mode",
                    label: "Mode",
                    kind: ParamKind::Choice(&["in", "out"]),
                    default: 0.0,
                    range: None,
                    soft: None,
                    unit: Unit::None,
                    scaling: Scaling::None,
                    keyframable: false,
                    derived: false,
                },
                ParamDesc {
                    field: "progress",
                    index: None,
                    key: "progress",
                    label: "Progress",
                    kind: ParamKind::Float,
                    default: 0.0,
                    range: Some((0.0, 1.0)),
                    soft: None,
                    unit: Unit::Ratio,
                    scaling: Scaling::None,
                    keyframable: false,
                    derived: true,
                },
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
        )),
        tag: "color_fade",
        sample: || BuiltinEffect::ColorFade { color: [0, 0, 0, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress: 0.0 },
    },
    EffectType {
        desc: colour(
            "klipp.fx.invert",
            "Invert",
            &[ParamDesc::float("mix", "Mix", 1.0, (0.0, 1.0), Unit::Ratio)],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
        ),
        tag: "invert",
        sample: || BuiltinEffect::Invert { mix: 1.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.posterize",
            "Posterize",
            &[ParamDesc::float("levels", "Levels", 8.0, (2.0, 256.0), Unit::Count)],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
        )),
        tag: "posterize",
        sample: || BuiltinEffect::Posterize { levels: 8.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.channel_mixer",
            "Channel Mixer",
            &[
                ParamDesc::float("red_red", "Red from red", 1.0, MIXER, Unit::Ratio),
                ParamDesc::float("red_green", "Red from green", 0.0, MIXER, Unit::Ratio),
                ParamDesc::float("red_blue", "Red from blue", 0.0, MIXER, Unit::Ratio),
                ParamDesc::float("green_red", "Green from red", 0.0, MIXER, Unit::Ratio),
                ParamDesc::float("green_green", "Green from green", 1.0, MIXER, Unit::Ratio),
                ParamDesc::float("green_blue", "Green from blue", 0.0, MIXER, Unit::Ratio),
                ParamDesc::float("blue_red", "Blue from red", 0.0, MIXER, Unit::Ratio),
                ParamDesc::float("blue_green", "Blue from green", 0.0, MIXER, Unit::Ratio),
                ParamDesc::float("blue_blue", "Blue from blue", 1.0, MIXER, Unit::Ratio),
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
        )),
        tag: "channel_mixer",
        sample: || BuiltinEffect::ChannelMixer {
            red_red: 1.0,
            red_green: 0.0,
            red_blue: 0.0,
            green_red: 0.0,
            green_green: 1.0,
            green_blue: 0.0,
            blue_red: 0.0,
            blue_green: 0.0,
            blue_blue: 1.0,
        },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.gamma",
            "Gamma",
            &[ParamDesc::float("gamma", "Gamma", 1.0, (0.1, 5.0), Unit::Ratio)],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
        )),
        tag: "gamma",
        sample: || BuiltinEffect::Gamma { gamma: 1.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.hsv_adjust",
            "HSV Adjust",
            &[
                ParamDesc::float("hue_shift", "Hue", 0.0, (-180.0, 180.0), Unit::Degrees),
                ParamDesc::float("saturation", "Saturation", 1.0, (0.0, 3.0), Unit::Ratio),
                ParamDesc::float("value", "Value", 1.0, (0.0, 3.0), Unit::Ratio),
            ],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
        )),
        tag: "h_s_v_adjust",
        sample: || BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 1.0, value: 1.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.vibrance",
            "Vibrance",
            &[ParamDesc::float("amount", "Amount", 0.0, (-1.0, 2.0), Unit::Ratio)],
            ColourDomain::Linear,
            Spatial::PerPixel,
        )),
        tag: "vibrance",
        sample: || BuiltinEffect::Vibrance { amount: 0.0 },
    },
    // The picture's brightness along a ramp of three colours (#2758),
    // read as it looks: on the display encoding.
    EffectType {
        desc: colour(
            "klipp.fx.gradient_map",
            "Gradient Map",
            &[
                ParamDesc { kind: ParamKind::Colour, range: None, soft: None, unit: Unit::None, keyframable: false, ..ParamDesc::float("shadow", "Shadows", f32::NAN, (0.0, 1.0), Unit::None) },
                ParamDesc { kind: ParamKind::Colour, range: None, soft: None, unit: Unit::None, keyframable: false, ..ParamDesc::float("mid", "Midtones", f32::NAN, (0.0, 1.0), Unit::None) },
                ParamDesc { kind: ParamKind::Colour, range: None, soft: None, unit: Unit::None, keyframable: false, ..ParamDesc::float("highlight", "Highlights", f32::NAN, (0.0, 1.0), Unit::None) },
                ParamDesc::float("mix", "Mix", 1.0, (0.0, 1.0), Unit::Ratio),
            ],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
        ),
        tag: "gradient_map",
        sample: || BuiltinEffect::GradientMap { shadow: [20, 24, 60], mid: [190, 90, 70], highlight: [255, 236, 190], mix: 1.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.levels",
            "Levels",
            &[
                ParamDesc::float("in_black", "Input black", 0.0, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("in_white", "Input white", 1.0, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("gamma", "Gamma", 1.0, (0.1, 5.0), Unit::Ratio),
                ParamDesc::float("out_black", "Output black", 0.0, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("out_white", "Output white", 1.0, (0.0, 1.0), Unit::Ratio),
            ],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
        )),
        tag: "levels",
        sample: || BuiltinEffect::Levels { in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.equalize",
            "Equalize",
            &[ParamDesc::float("strength", "Strength", 1.0, (0.0, 1.0), Unit::Ratio)],
            ColourDomain::DisplayEncoded,
            Spatial::Global,
        )),
        tag: "equalize",
        sample: || BuiltinEffect::Equalize { strength: 1.0 },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.lut_3d",
            "3D LUT",
            &[
                ParamDesc { kind: ParamKind::Other, soft: None, ..ParamDesc::float("path", "LUT file", f32::NAN, (0.0, 0.0), Unit::None).fixed() },
                ParamDesc::float("opacity", "Opacity", 1.0, (0.0, 1.0), Unit::Ratio),
                switch("tetrahedral", "Tetrahedral", true),
                prepared(),
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
        )),
        tag: "lut_3d",
        sample: || BuiltinEffect::Lut3D { path: String::new(), opacity: 1.0, tetrahedral: true, prepared: None },
    },
    EffectType {
        desc: mixable(colour(
            "klipp.fx.colour_space_transform",
            "Colour Space Transform",
            &[
                choice("input_gamut", "Input gamut", GAMUTS),
                choice("input_curve", "Input curve", CURVES),
                choice("output_gamut", "Output gamut", GAMUTS),
                choice("output_curve", "Output curve", CURVES),
                prepared(),
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
        )),
        tag: "colour_space_transform",
        sample: || BuiltinEffect::ColourSpaceTransform { input_gamut: 0, input_curve: 0, output_gamut: 0, output_curve: 0, prepared: None },
    },
];

/// An on or off switch.
const fn switch(field: &'static str, label: &'static str, on: bool) -> ParamDesc {
    ParamDesc {
        kind: ParamKind::Bool,
        soft: None,
        ..ParamDesc::float(field, label, if on { 1.0 } else { 0.0 }, (0.0, 1.0), Unit::None).fixed()
    }
}

/// One of `names`, saved by its index, the first by default.
const fn choice(field: &'static str, label: &'static str, names: &'static [&'static str]) -> ParamDesc {
    ParamDesc {
        kind: ParamKind::Choice(names),
        soft: None,
        ..ParamDesc::float(field, label, 0.0, (0.0, 0.0), Unit::None).fixed()
    }
}

/// What the renderer works out for a LUT or a transform before each run
/// ([`crate::colour_transform`]): never saved, never shown.
const fn prepared() -> ParamDesc {
    ParamDesc {
        kind: ParamKind::Other,
        soft: None,
        derived: true,
        ..ParamDesc::float("prepared", "Prepared", f32::NAN, (0.0, 0.0), Unit::None).fixed()
    }
}
