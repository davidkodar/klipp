//! The blur, sharpen and detail effects (#1005).
//!
//! Defaults are what resetting the effect gives (the inspector's
//! `builtin_effect_default`); soft ranges are the inspector sliders'.
//! No hard ranges: where a value is limited, the computation clamps it
//! (a sample count to 3..32 or 4..32) rather than refusing it. All of
//! them act on linear light and read the pixels around each one.
//!
//! Where two places disagree, the description follows the code that
//! renders and says so:
//!
//! * Sharpen resets to 0 (no change), but its slider's double-click
//!   reset is 0.5.
//! * Median filter's slider reaches a radius of 5 and the CPU honours
//!   it, but the GPU clamps the radius to 2 (`gpu.rs`), so above 2 the
//!   two paths differ.
//! * Focus blur has no inspector controls: the compositor puts it on a
//!   title layer from its roll's focus band, so it has no slider ranges.

use super::{
    mixable,
    Alpha, Category, ColourDomain, EffectType, NodeTypeDesc, ParamDesc, ParamKind, Scaling, Spatial, Temporal, Unit, IMAGE_IN,
    IMAGE_OUT,
};
use crate::BuiltinEffect;

const fn blur(id: &'static str, name: &'static str, params: &'static [ParamDesc]) -> NodeTypeDesc {
    NodeTypeDesc {
        id,
        name,
        category: Category::BlurDetail,
        inputs: IMAGE_IN,
        outputs: IMAGE_OUT,
        params,
        domain: ColourDomain::Linear,
        alpha: Alpha::Straight,
        spatial: Spatial::Neighbourhood,
        temporal: Temporal::None,
        draws_outside: false,
        gpu: true,
        cpu: true,
        mixable: false,
    }
}

/// A whole-number parameter: a sample count, a pass count, a radius in
/// whole pixels (which the effect scales with the render size).
const fn whole(field: &'static str, label: &'static str, default: f32, soft: Option<(f32, f32)>, unit: Unit, keyframable: bool) -> ParamDesc {
    ParamDesc {
        field,
        index: None,
        key: field,
        label,
        kind: ParamKind::Whole,
        default,
        range: None,
        soft,
        unit,
        scaling: if matches!(unit, Unit::CanvasPx) { Scaling::AtRenderScale } else { Scaling::None },
        keyframable,
        derived: false,
    }
}

/// A pixel size never scaled below one pixel.
const fn at_least_one_px(mut p: ParamDesc) -> ParamDesc {
    p.scaling = Scaling::AtRenderScaleAtLeastOne;
    p
}

/// A parameter with no inspector slider.
const fn unslid(mut p: ParamDesc) -> ParamDesc {
    p.soft = None;
    p
}

pub(super) static TYPES: &[EffectType] = &[
    EffectType {
        desc: mixable(blur(
            "klipp.fx.gaussian_blur",
            "Gaussian Blur",
            &[ParamDesc::float("radius", "Radius", 0.0, (0.0, 100.0), Unit::CanvasPx)],
        )),
        tag: "gaussian_blur",
        sample: || BuiltinEffect::GaussianBlur { radius: 0.0 },
    },
    EffectType {
        desc: mixable(blur(
            "klipp.fx.focus_blur",
            "Focus Blur",
            &[
                unslid(ParamDesc::float("radius", "Radius", 8.0, (0.0, 0.0), Unit::CanvasPx).fixed().within((0.0, 400.0))),
                ParamDesc {
                    field: "vertical",
                    index: None,
                    key: "vertical",
                    label: "Vertical",
                    kind: ParamKind::Bool,
                    default: 1.0,
                    range: None,
                    soft: None,
                    unit: Unit::None,
                    scaling: Scaling::None,
                    keyframable: false,
                    derived: false,
                },
                unslid(ParamDesc::float("band", "Band", 0.3, (0.0, 0.0), Unit::Normalised).fixed().within((0.0, 1.0))),
            ],
        )),
        tag: "focus_blur",
        sample: || BuiltinEffect::FocusBlur { radius: 8.0, vertical: true, band: 0.3 },
    },
    EffectType {
        desc: mixable(blur("klipp.fx.sharpen", "Sharpen", &[ParamDesc::float("amount", "Amount", 0.0, (0.0, 2.0), Unit::Ratio)])),
        tag: "sharpen",
        sample: || BuiltinEffect::Sharpen { amount: 0.0 },
    },
    EffectType {
        desc: mixable(blur(
            "klipp.fx.directional_blur",
            "Directional Blur",
            &[
                ParamDesc::float("angle", "Angle", 0.0, (0.0, 360.0), Unit::Degrees),
                ParamDesc::float("distance", "Distance", 20.0, (0.0, 100.0), Unit::CanvasPx),
                whole("samples", "Samples", 8.0, Some((3.0, 32.0)), Unit::Count, true),
            ],
        )),
        tag: "directional_blur",
        sample: || BuiltinEffect::DirectionalBlur { angle: 0.0, distance: 20.0, samples: 8 },
    },
    EffectType {
        desc: mixable(blur(
            "klipp.fx.radial_blur",
            "Radial Blur",
            &[
                ParamDesc::float("center_x", "Center X", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("center_y", "Center Y", 0.5, (0.0, 1.0), Unit::Normalised),
                // A percentage of the distance to the centre when
                // zooming, half a degree per step when spinning.
                ParamDesc::float("amount", "Amount", 10.0, (0.0, 50.0), Unit::Ratio),
                whole("samples", "Samples", 12.0, Some((4.0, 32.0)), Unit::Count, false),
                ParamDesc {
                    field: "mode",
                    index: None,
                    key: "mode",
                    label: "Mode",
                    kind: ParamKind::Choice(&["zoom", "spin"]),
                    default: 0.0,
                    range: None,
                    soft: None,
                    unit: Unit::None,
                    scaling: Scaling::None,
                    keyframable: false,
                    derived: false,
                },
            ],
        )),
        tag: "radial_blur",
        sample: || BuiltinEffect::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 12, mode: 0 },
    },
    EffectType {
        desc: mixable(blur(
            "klipp.fx.tilt_shift",
            "Tilt-Shift",
            &[
                ParamDesc::float("center", "Focus", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("width", "Band", 0.3, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("softness", "Softness", 0.2, (0.01, 0.5), Unit::Normalised),
                ParamDesc::float("radius", "Blur", 12.0, (0.0, 60.0), Unit::CanvasPx),
            ],
        )),
        tag: "tilt_shift",
        sample: || BuiltinEffect::TiltShift { center: 0.5, width: 0.3, softness: 0.2, radius: 12.0 },
    },
    EffectType {
        desc: mixable(blur(
            "klipp.fx.denoise",
            "Denoise",
            &[
                ParamDesc::float("spatial_sigma", "Spatial Sigma", 5.0, (1.0, 20.0), Unit::CanvasPx),
                ParamDesc::float("color_sigma", "Colour Sigma", 0.1, (0.01, 1.0), Unit::Ratio),
                whole("iterations", "Iterations", 1.0, Some((1.0, 3.0)), Unit::Count, true),
            ],
        )),
        tag: "denoise",
        sample: || BuiltinEffect::Denoise { spatial_sigma: 5.0, color_sigma: 0.1, iterations: 1 },
    },
    EffectType {
        desc: mixable(blur(
            "klipp.fx.median_filter",
            "Median Filter",
            &[whole("radius", "Radius", 1.0, Some((1.0, 5.0)), Unit::CanvasPx, false)],
        )),
        tag: "median_filter",
        sample: || BuiltinEffect::MedianFilter { radius: 1 },
    },
    EffectType {
        desc: mixable(blur("klipp.fx.dilate", "Dilate", &[whole("radius", "Radius", 1.0, Some((1.0, 10.0)), Unit::CanvasPx, false)])),
        tag: "dilate",
        sample: || BuiltinEffect::Dilate { radius: 1 },
    },
    EffectType {
        desc: mixable(blur("klipp.fx.erode", "Erode", &[whole("radius", "Radius", 1.0, Some((1.0, 10.0)), Unit::CanvasPx, false)])),
        tag: "erode",
        sample: || BuiltinEffect::Erode { radius: 1 },
    },
    EffectType {
        desc: blur(
            "klipp.fx.emboss",
            "Emboss",
            &[
                ParamDesc::float("angle", "Angle", 135.0, (0.0, 360.0), Unit::Degrees),
                ParamDesc::float("strength", "Strength", 1.0, (0.0, 5.0), Unit::Ratio),
                ParamDesc::float("mix", "Mix", 1.0, (0.0, 1.0), Unit::Ratio),
            ],
        ),
        tag: "emboss",
        sample: || BuiltinEffect::Emboss { angle: 135.0, strength: 1.0, mix: 1.0 },
    },
    EffectType {
        desc: blur(
            "klipp.fx.edge_detect",
            "Edge Detect",
            &[
                ParamDesc::float("strength", "Strength", 1.0, (0.0, 5.0), Unit::Ratio),
                ParamDesc::float("mix", "Mix", 1.0, (0.0, 1.0), Unit::Ratio),
            ],
        ),
        tag: "edge_detect",
        sample: || BuiltinEffect::EdgeDetect { strength: 1.0, mix: 1.0 },
    },
    EffectType {
        desc: mixable(blur("klipp.fx.mosaic", "Mosaic", &[at_least_one_px(ParamDesc::float("block_size", "Block Size", 10.0, (1.0, 100.0), Unit::CanvasPx))])),
        tag: "mosaic",
        sample: || BuiltinEffect::Mosaic { block_size: 10.0 },
    },
];
