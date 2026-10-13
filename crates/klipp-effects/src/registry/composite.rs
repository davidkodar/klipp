//! The effects that bring a second picture in (#1746): a Merge lays the
//! picture wired into its `fg` port over the clip's; a Generator makes a
//! picture (a solid, a gradient) for it from nothing; a Media Source
//! brings another media item's.

use super::{Alpha, Category, ColourDomain, EffectType, NodeTypeDesc, ParamDesc, ParamKind, PortDesc, PortType, Spatial, Temporal, Unit, IMAGE_IN, IMAGE_OUT};
use crate::generator::SHAPES;
use crate::{BlendMode, BuiltinEffect};

/// A merge's inputs: the picture so far, and the one laid over it.
pub const MERGE_IN: &[PortDesc] = &[
    PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] },
    PortDesc { key: "fg", ty: PortType::Image, optional: true, params: &[] },
];

/// The blend modes a merge lays its picture over by, as saved.
pub const BLEND_MODES: &[&str] = &["Normal", "Add", "Multiply", "Screen", "Overlay", "SoftLight", "HardLight", "Difference", "Exclusion", "ColorDodge", "ColorBurn", "Darken", "Lighten"];

const fn composite(id: &'static str, name: &'static str, inputs: &'static [PortDesc], params: &'static [ParamDesc], spatial: Spatial) -> NodeTypeDesc {
    NodeTypeDesc {
        id,
        name,
        category: Category::Composite,
        inputs,
        outputs: IMAGE_OUT,
        params,
        domain: ColourDomain::Linear,
        alpha: Alpha::Straight,
        spatial,
        temporal: Temporal::None,
        draws_outside: false,
        gpu: true,
        cpu: true,
        mixable: false,
    }
}

/// One of `names`, saved by name, the first by default.
const fn choice(field: &'static str, label: &'static str, names: &'static [&'static str]) -> ParamDesc {
    ParamDesc { kind: ParamKind::Choice(names), soft: None, ..ParamDesc::float(field, label, 0.0, (0.0, 0.0), Unit::None).fixed() }
}

/// An authored colour, sRGB bytes with alpha.
const fn colour(field: &'static str, label: &'static str) -> ParamDesc {
    ParamDesc { kind: ParamKind::Colour, soft: None, default: f32::NAN, ..ParamDesc::float(field, label, 0.0, (0.0, 0.0), Unit::None).fixed() }
}

pub(super) static TYPES: &[EffectType] = &[
    EffectType {
        desc: composite(
            "klipp.fx.merge",
            "Merge",
            MERGE_IN,
            &[choice("blend_mode", "Blend", BLEND_MODES), ParamDesc::float("opacity", "Opacity", 1.0, (0.0, 1.0), Unit::Ratio)],
            Spatial::PerPixel,
        ),
        tag: "merge",
        sample: || BuiltinEffect::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 },
    },
    EffectType {
        desc: composite(
            "klipp.fx.generator",
            "Generator",
            IMAGE_IN,
            &[
                ParamDesc { kind: ParamKind::Choice(SHAPES), soft: None, ..ParamDesc::float("shape", "Shape", 0.0, (0.0, 0.0), Unit::None).fixed() },
                colour("color_a", "Colour"),
                colour("color_b", "To colour"),
                ParamDesc::float("angle", "Angle", 0.0, (-180.0, 180.0), Unit::Degrees),
                ParamDesc::float("center_x", "Centre X", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("center_y", "Centre Y", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("radius", "Radius", 0.5, (0.01, 2.0), Unit::Ratio),
            ],
            Spatial::Global,
        ),
        tag: "generator",
        sample: || BuiltinEffect::Generator { shape: 0, color_a: [40, 90, 200, 255], color_b: [255, 255, 255, 255], angle: 0.0, center_x: 0.5, center_y: 0.5, radius: 0.5 },
    },
    EffectType {
        desc: NodeTypeDesc {
            // Read by the executor from its frame context, on the CPU.
            gpu: false,
            ..composite(
                "klipp.fx.media_source",
                "Media Source",
                IMAGE_IN,
                &[
                    ParamDesc { kind: ParamKind::Other, soft: None, ..ParamDesc::float("media", "Media", f32::NAN, (0.0, 0.0), Unit::None).fixed() },
                    ParamDesc::float("offset", "Offset", 0.0, (-3600.0, 3600.0), Unit::Seconds).fixed(),
                ],
                Spatial::Global,
            )
        },
        tag: "media_source",
        sample: || BuiltinEffect::MediaSource { media: String::new(), offset: 0.0 },
    },
];
