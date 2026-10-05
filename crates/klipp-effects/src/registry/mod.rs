//! One description of every built-in effect: what it takes in, what it
//! gives out, and every parameter with its type, default, range, unit and
//! whether it can be keyframed (#1003).
//!
//! What an effect is used to be spread across `match` statements that
//! each knew one thing: the renderer's keyframe list
//! ([`BuiltinEffect::animate`]), which parameters scale with the render
//! size ([`BuiltinEffect::at_render_scale`]), which values are neutral
//! ([`crate::is_identity_effect`]), which effects draw outside the
//! picture ([`crate::draws_outside_the_picture`]), and the inspector's
//! panels. When two of them disagreed nothing said so, and #992 was the
//! result: keyframes the inspector offered and the render ignored.
//!
//! The registry says all of it in one place. Nothing reads it yet. For
//! now it is checked *against* those matches by
//! `tests/registry_coverage.rs`, so it can only describe what the code
//! already does; a description that disagrees fails there. Families are
//! added one module at a time (#1004-#1008), each moving its variants off
//! the test's list of effects not yet described.
//!
//! The grade's node types are described with the same types in
//! `klipp_nodes::registry` (#1009).

use crate::BuiltinEffect;

mod colour;
mod film;
mod geometry;

/// What kind of operation a node type is, for grouping in the browser and
/// the add menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Changes colour or tone pixel by pixel.
    Colour,
    /// Blurs, sharpens or otherwise reads neighbouring pixels.
    BlurDetail,
    /// Adds light: glows, flares, halation, shadows.
    LightGlow,
    /// Moves, crops, warps or frames the picture.
    Geometry,
    /// Film and video looks, noise, and effects that read other frames.
    FilmTemporal,
    /// Keys a colour out.
    Key,
    /// Runs a plug-in.
    PlugIn,
    /// A grade node.
    Grade,
}

/// What travels along a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    /// A picture: linear RGBA in the working space, straight alpha.
    Image,
    /// A single-channel mask, carried as a picture whose red channel is
    /// the mask (as grade mattes are today).
    Matte,
}

/// One input or output of a node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortDesc {
    /// Stable name, never an index: `"in"`, `"out"`, `"mask"`, `"bg"`.
    pub key: &'static str,
    pub ty: PortType,
    /// Whether the node works with nothing connected here.
    pub optional: bool,
}

/// The single picture input every built-in effect has today.
pub const IMAGE_IN: &[PortDesc] = &[PortDesc { key: "in", ty: PortType::Image, optional: false }];
/// The single picture output every built-in effect has today.
pub const IMAGE_OUT: &[PortDesc] = &[PortDesc { key: "out", ty: PortType::Image, optional: false }];

/// What kind of value a parameter holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKind {
    /// A real number.
    Float,
    /// A whole number stored as `u32`: a sample count, a radius in whole
    /// pixels.
    Whole,
    /// On or off.
    Bool,
    /// One of a fixed set, in the order the code numbers them. The
    /// default is the index. Saved either by name (an enum) or by its
    /// index (a `u32` the code matches on).
    Choice(&'static [&'static str]),
    /// An authored colour, sRGB bytes.
    Colour,
    /// Anything the types above do not cover yet (a path, a plug-in's
    /// values). Described, never keyframable.
    Other,
}

/// What a parameter's number means, which decides how it behaves when the
/// picture is rendered at another size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// A plain number: an amount, a multiplier, a mix.
    Ratio,
    /// An angle.
    Degrees,
    /// A length of time.
    Seconds,
    /// A fraction of the frame's width or height (a crop edge, a centre).
    Normalised,
    /// Pixels of the sequence's frame. Scaled by the render size, exactly
    /// as [`BuiltinEffect::at_render_scale`] scales it.
    CanvasPx,
    /// Pixels of a 1080-line frame, whatever the sequence's size.
    Per1080,
    /// A count: levels, samples, iterations.
    Count,
    /// No unit: a colour, a choice, a switch.
    None,
}

/// What converts a pixel-sized parameter to the size a frame is rendered
/// at. A parameter's [`Unit`] says what its number means; this says who
/// makes it mean the same at every render size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scaling {
    /// Nothing: the number is the same at every size.
    None,
    /// The effect itself, through [`BuiltinEffect::at_render_scale`].
    AtRenderScale,
    /// The compositor, which resolves the clip's placement in the
    /// sequence's pixels and converts the offset to the rendered frame's
    /// (`klipp_compose::render::to_render_pixels`).
    Placement,
}

/// One parameter of a node type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDesc {
    /// The field's name in the saved effect (its serde name).
    pub field: &'static str,
    /// Which element of the field, when the field is an array that is
    /// keyframed element by element (a corner pin's corners are saved as
    /// `[x, y]` and keyed `tl_x`, `tl_y`). `None` for a plain field.
    pub index: Option<usize>,
    /// The name its keyframes are stored under. The same as `field`
    /// except where history says otherwise (chroma key's soft edge is
    /// keyed `"spill"`).
    pub key: &'static str,
    /// What the inspector calls it.
    pub label: &'static str,
    pub kind: ParamKind,
    /// The value it has when the effect is reset. `NAN` for kinds with no
    /// single number (`Colour`, `Other`).
    pub default: f32,
    /// The values the effect accepts. `None` where it accepts any number
    /// and the computation clamps (or does not need to).
    pub range: Option<(f32, f32)>,
    /// The inspector slider's range: where the useful values are. The
    /// default may lie outside it when the reset value is a marker (a
    /// transform's perspective resets to 0, "unset", read as 1); the
    /// module describing such a parameter says so.
    pub soft: Option<(f32, f32)>,
    pub unit: Unit,
    /// Who converts it to the rendered size. Every [`Unit::CanvasPx`]
    /// parameter names one; [`Scaling::AtRenderScale`] must agree with
    /// [`BuiltinEffect::at_render_scale`].
    pub scaling: Scaling,
    /// Whether it can be keyframed. Must agree with
    /// [`BuiltinEffect::animate`].
    pub keyframable: bool,
    /// Written by the compositor every frame rather than by the user (a
    /// fade's progress, a grain seed). Not shown and not saved.
    pub derived: bool,
}

impl ParamDesc {
    /// A real-valued parameter that is keyframable, with no hard range,
    /// the slider's range as given: the common case.
    pub const fn float(field: &'static str, label: &'static str, default: f32, soft: (f32, f32), unit: Unit) -> Self {
        ParamDesc {
            field,
            index: None,
            key: field,
            label,
            kind: ParamKind::Float,
            default,
            range: None,
            soft: Some(soft),
            unit,
            scaling: if matches!(unit, Unit::CanvasPx) { Scaling::AtRenderScale } else { Scaling::None },
            keyframable: true,
            derived: false,
        }
    }

    /// The same parameter as one element of an array field, keyframed
    /// under `key`.
    pub const fn element(mut self, index: usize, key: &'static str) -> Self {
        self.index = Some(index);
        self.key = key;
        self
    }

    /// The same parameter, converted to the rendered size by `scaling`.
    pub const fn scaled_by(mut self, scaling: Scaling) -> Self {
        self.scaling = scaling;
        self
    }

    /// The same parameter with values outside `range` refused.
    pub const fn within(mut self, range: (f32, f32)) -> Self {
        self.range = Some(range);
        self
    }

    /// The same parameter, not keyframable.
    pub const fn fixed(mut self) -> Self {
        self.keyframable = false;
        self
    }
}

/// Which encoding of the picture a node computes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourDomain {
    /// Linear light in the working space.
    Linear,
    /// The display-encoded picture (sRGB): the perceptual adjustments
    /// (invert, posterize, levels, gamma, HSV, equalize; `cpu.rs`).
    DisplayEncoded,
    /// The grade's encoding (BT.1886, or ACEScct/ACEScc under ACEScg).
    GradeEncoded,
}

/// How a node treats alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alpha {
    /// Computes on straight (unpremultiplied) colour, as every effect does
    /// today.
    Straight,
    /// Needs premultiplied colour.
    Premultiplied,
}

/// How much of the picture a node reads to make one output pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spatial {
    /// Only the same pixel.
    PerPixel,
    /// Pixels around it.
    Neighbourhood,
    /// The whole frame (a histogram, a mean).
    Global,
    /// Pixels from elsewhere, by a mapping (a transform, a warp).
    Geometric,
}

/// What a node needs from other times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Temporal {
    /// Nothing: the current frame only.
    None,
    /// Neighbouring frames of the source media (temporal noise reduction,
    /// frame blending).
    SourceNeighbours,
    /// Per-frame results of an analysis of the clip (deflicker gains, a
    /// stabilisation trajectory, rolling-shutter motion).
    AnalysisDriven,
}

/// One node type: a built-in effect, or (in `klipp_nodes::registry`) a
/// grade node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeTypeDesc {
    /// Stable identifier: `"klipp.fx.gaussian_blur"`.
    pub id: &'static str,
    /// What the browser and the inspector call it.
    pub name: &'static str,
    pub category: Category,
    pub inputs: &'static [PortDesc],
    pub outputs: &'static [PortDesc],
    pub params: &'static [ParamDesc],
    pub domain: ColourDomain,
    pub alpha: Alpha,
    pub spatial: Spatial,
    pub temporal: Temporal,
    /// Draws into the transparent margin around a placed layer (a shadow,
    /// an outer glow). Must agree with
    /// [`crate::draws_outside_the_picture`].
    pub draws_outside: bool,
    /// Has a GPU implementation.
    pub gpu: bool,
    /// Has a CPU implementation.
    pub cpu: bool,
}

/// A built-in effect's description, tied to its variant.
#[derive(Debug, Clone, Copy)]
pub struct EffectType {
    pub desc: NodeTypeDesc,
    /// The variant's tag in a saved project (`"gaussian_blur"`).
    pub tag: &'static str,
    /// The effect as a reset leaves it: every parameter at its
    /// [`ParamDesc::default`].
    pub sample: fn() -> BuiltinEffect,
}

/// Every built-in effect described so far.
pub fn effect_types() -> impl Iterator<Item = &'static EffectType> {
    colour::TYPES.iter().chain(geometry::TYPES).chain(film::TYPES)
}

/// The description of the effect saved under `tag`, if it is described.
pub fn effect_type(tag: &str) -> Option<&'static EffectType> {
    effect_types().find(|t| t.tag == tag)
}
