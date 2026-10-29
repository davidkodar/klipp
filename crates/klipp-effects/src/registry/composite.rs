//! The effects that bring a second picture in (#1746): a Merge lays the
//! picture wired into its `fg` port over the clip's; a Generator makes a
//! picture (a solid, a gradient) for it from nothing; a Media Source
//! brings another media item's, and Clip Below what is under the clip.

use super::{hidden, Alpha, Category, ColourDomain, EffectType, NodeTypeDesc, ParamDesc, ParamKind, PortDesc, PortType, Spatial, Temporal, Unit, IMAGE_IN, IMAGE_OUT};
use crate::generator::SHAPES;
use crate::{BlendMode, BuiltinEffect};

/// A merge's inputs: the picture so far, the one laid over it, and a
/// mask limiting where it is laid (#2758): the merge's result mixed back
/// over the picture so far by the mask, as a masked effect's is.
pub const MERGE_IN: &[PortDesc] = &[
    PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] },
    PortDesc { key: "fg", ty: PortType::Image, optional: true, params: &[] },
    PortDesc { key: "mask", ty: PortType::Matte, optional: true, params: &[super::MASK_CHANNEL] },
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
        browser: hidden("Lays a second picture over the chain from a port beside it, so it is added in the Composite page's graph"),
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
        browser: hidden("A source picture beside the chain for a Merge, added from the Composite page's source menu"),
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
        browser: hidden("A source picture beside the chain for a Merge, added from the Composite page's source menu"),
        sample: || BuiltinEffect::MediaSource { media: String::new(), offset: 0.0 },
    },
    EffectType {
        desc: NodeTypeDesc {
            // Drawn by the renderer as a title is, read by the executor
            // from its frame context.
            gpu: false,
            ..composite(
                "klipp.fx.title_source",
                "Title Source",
                IMAGE_IN,
                &[ParamDesc { kind: ParamKind::Other, soft: None, ..ParamDesc::float("title", "Title", f32::NAN, (0.0, 0.0), Unit::None).fixed() }],
                Spatial::Global,
            )
        },
        tag: "title_source",
        browser: hidden("A source picture beside the chain for a Merge, added from the Composite page's source menu"),
        sample: || BuiltinEffect::TitleSource { title: String::new() },
    },
    EffectType {
        desc: NodeTypeDesc {
            // Drawn by the executor from what the render composited below.
            gpu: false,
            ..composite("klipp.fx.clip_below", "Clip Below", IMAGE_IN, &[], Spatial::Global)
        },
        tag: "clip_below",
        browser: hidden("A source picture beside the chain for a Merge, added from the Composite page's source menu"),
        sample: || BuiltinEffect::ClipBelow {},
    },
    EffectType {
        desc: NodeTypeDesc {
            // A grade, run by the executor as a compound of grade nodes:
            // never an effect a menu offers.
            category: Category::Grade,
            gpu: false,
            ..composite("klipp.fx.grade_compound", "Grade", IMAGE_IN, &[], Spatial::Global)
        },
        tag: "grade_compound",
        browser: hidden("Marks where the clip's grade runs in its chain; the Colour page makes it"),
        sample: || BuiltinEffect::GradeCompound {},
    },
];
