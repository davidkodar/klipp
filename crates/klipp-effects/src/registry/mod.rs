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

mod blur;
mod colour;
mod composite;
pub use composite::{BLEND_MODES, MERGE_IN};
mod film;
mod geometry;
mod light;
pub mod presentation;

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
    /// Brings a second picture in: makes one, or lays one over (#1746).
    Composite,
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
    /// Settings of what arrives here, saved with the node as parameters
    /// under [`port_param_key`] (`"mask.channel"`).
    pub params: &'static [PortParamDesc],
}

/// A setting of an input, chosen from named options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortParamDesc {
    pub key: &'static str,
    pub label: &'static str,
    /// The options, as saved.
    pub options: &'static [&'static str],
    /// The option a new connection starts with.
    pub default: &'static str,
    /// The option a connection carried over from before the setting
    /// existed keeps: what it always meant.
    pub migrated: &'static str,
}

/// The key a port's setting is saved under among the node's parameters.
pub fn port_param_key(port: &str, param: &str) -> String {
    format!("{port}.{param}")
}

/// Which part of a picture arriving at a mask input is the mask (#1089,
/// M2). Luminance is read in the picture's linear working space. Mattes
/// carried over from revision 1 have always been their red channel.
///
/// **An absent setting reads as [`PortParamDesc::migrated`] (red), never
/// as [`PortParamDesc::default`].** `default` is only what a new connection
/// writes, explicitly; a graph lifted from revision 1 has no setting, and
/// reading it as luminance would change the picture of every one of them.
pub const MASK_CHANNEL: PortParamDesc = PortParamDesc {
    key: "channel",
    label: "Channel",
    options: &["red", "luminance", "alpha"],
    default: "luminance",
    migrated: "red",
};

/// A grade correction's gate channel (M3 step 17a): as [`MASK_CHANNEL`],
/// except that a new gate connection writes none, so a gate reads red, as
/// every gate always has, until a channel is picked; its default is red.
pub const GATE_CHANNEL: PortParamDesc = PortParamDesc { default: "red", ..MASK_CHANNEL };

/// The single picture input every built-in effect has.
pub const IMAGE_IN: &[PortDesc] = &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }];
/// A mixable node's inputs: its picture, and an optional mask limiting
/// where its result replaces the picture.
pub const MIXABLE_IN: &[PortDesc] = &[
    PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] },
    PortDesc { key: "mask", ty: PortType::Matte, optional: true, params: &[MASK_CHANNEL] },
];
/// A gateable grade correction's inputs: its picture, and the optional
/// matte gating it, with its channel setting.
pub const GATE_IN: &[PortDesc] = &[
    PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] },
    PortDesc { key: "mask", ty: PortType::Matte, optional: true, params: &[GATE_CHANNEL] },
];
/// The single picture output every built-in effect has.
pub const IMAGE_OUT: &[PortDesc] = &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }];

/// `desc` as a mixable node: it takes a `mask` and a `mix` amount, and
/// its result is blended over its input by them (#1089).
pub const fn mixable(desc: NodeTypeDesc) -> NodeTypeDesc {
    NodeTypeDesc { mixable: true, inputs: MIXABLE_IN, ..desc }
}

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
    /// Pixels of the sequence's frame, converted to the rendered size by
    /// whoever its [`Scaling`] names.
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
    /// As [`Scaling::AtRenderScale`], never below one pixel (a mosaic's
    /// block).
    AtRenderScaleAtLeastOne,
    /// The compositor, which resolves the clip's placement in the
    /// sequence's pixels and converts the offset to the rendered frame's
    /// (`klipp_compose::render::to_render_pixels`).
    Placement,
    /// The grade executor, by the render scale the compositor and the
    /// node preview set (`GraphExecutor::set_render_scale`): a grade
    /// node's pixel sizes (#1009).
    Executor,
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

    /// The values the effect is safe to render with (#2995), whatever
    /// writes them: its declared range, else four times the slider's
    /// reach either side of zero, a slider that starts at zero or above
    /// starting there. A typed value may go past the slider; a crafted
    /// one may not make the render run or allocate without bound. `None`
    /// for a number with neither (a test holds there are none) and for
    /// the kinds that aren't numbers.
    pub fn hard(&self) -> Option<(f32, f32)> {
        if !matches!(self.kind, ParamKind::Float | ParamKind::Whole) {
            return None;
        }
        if let Some(r) = self.range {
            return Some(r);
        }
        // An angle comes round: any turn is safe, and a typed -45° is not
        // a slider's 0.
        if self.unit == Unit::Degrees {
            return Some(ANGLE_RANGE);
        }
        let (lo, hi) = self.soft?;
        Some((if lo < 0.0 { lo * 4.0 } else { lo }, if hi > 0.0 { hi * 4.0 } else { hi }))
    }
}

/// The hard range of a clip-local time the compositor sets each frame:
/// a day either way.
pub const TIME_RANGE: (f32, f32) = (-86_400.0, 86_400.0);

/// The hard range of an angle: a hundred turns either way.
pub const ANGLE_RANGE: (f32, f32) = (-36_000.0, 36_000.0);

/// The hard range of a pattern seed: the whole numbers an f32 holds
/// exactly.
pub const SEED_RANGE: (f32, f32) = (0.0, 16_777_216.0);

/// A value the guard changed (#2995): the parameter, what was asked for,
/// and what it is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guarded {
    pub key: &'static str,
    pub was: f64,
    pub now: f32,
}

/// What the guard does with a number that isn't one (NaN, an infinity,
/// past f32's reach): refuses it, for an edit that can say why, or puts
/// the parameter's default in its place, for a file or a render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFinite {
    Refuse,
    Default,
}

/// The parameter of effect `tag` a value is written to by `key`: its
/// keyframe name, or its field.
fn param_of(tag: &str, key: &str) -> Option<&'static ParamDesc> {
    let ty = effect_type(tag)?;
    ty.desc.params.iter().find(|d| d.key == key).or_else(|| ty.desc.params.iter().find(|d| d.field == key && d.index.is_none()))
}

/// The one guard on a number written to an effect's parameter (#2995),
/// from any source: inside the parameter's [`ParamDesc::hard`] range,
/// and finite. `Ok` with the value to store and whether it was changed;
/// `Err` for a number that isn't one under [`NotFinite::Refuse`]. A key
/// that names no number of the effect passes as it is.
pub fn guard_number(tag: &str, key: &str, value: f64, not_finite: NotFinite) -> Result<(f32, Option<Guarded>), String> {
    let Some(desc) = param_of(tag, key).filter(|d| d.hard().is_some()) else {
        return Ok((value as f32, None));
    };
    let (lo, hi) = desc.hard().expect("filtered above");
    let as_f32 = value as f32;
    if !as_f32.is_finite() {
        return match not_finite {
            NotFinite::Refuse => Err(format!("{tag}'s {} takes a finite number, not {value}", desc.key)),
            NotFinite::Default => Ok((desc.default, Some(Guarded { key: desc.key, was: value, now: desc.default }))),
        };
    }
    // A whole number's key may sit between two: the render rounds it.
    let now = as_f32.clamp(lo, hi);
    Ok((now, (now != as_f32).then_some(Guarded { key: desc.key, was: value, now })))
}

/// [`guard_number`] over every number of an effect's saved entry (its
/// `type` names it), in place: what it changed, or why it was refused. A
/// number missing, or not a number where one is meant, is left for the
/// entry's own reading to say.
pub fn guard_entry(entry: &mut serde_json::Value, not_finite: NotFinite) -> Result<Vec<Guarded>, String> {
    let Some(tag) = entry.get("type").and_then(|t| t.as_str()).map(str::to_owned) else { return Ok(Vec::new()) };
    let Some(ty) = effect_type(&tag) else { return Ok(Vec::new()) };
    let mut changed = Vec::new();
    for desc in ty.desc.params.iter().filter(|d| d.hard().is_some() && !d.derived) {
        let slot = match desc.index {
            Some(i) => entry.get_mut(desc.field).and_then(|a| a.get_mut(i)),
            None => entry.get_mut(desc.field),
        };
        let Some(slot) = slot else { continue };
        let value = match slot {
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(f64::INFINITY),
            // A float field saved as null was not a number (serde writes
            // NaN and the infinities so).
            serde_json::Value::Null => f64::NAN,
            _ => continue,
        };
        let (now, guarded) = guard_number(&tag, desc.key, value, not_finite)?;
        if let Some(g) = guarded {
            // The field holds a whole number.
            *slot = if desc.kind == ParamKind::Whole { serde_json::json!(now.round() as i64) } else { serde_json::json!(now) };
            changed.push(Guarded { now: if desc.kind == ParamKind::Whole { now.round() } else { now }, ..g });
        }
    }
    Ok(changed)
}

/// [`guard_entry`] on a built effect, for a file's effects read whole:
/// not-numbers given their defaults. What it changed.
pub fn guard_effect(effect: &mut crate::BuiltinEffect) -> Vec<Guarded> {
    let Ok(mut entry) = serde_json::to_value(&*effect) else { return Vec::new() };
    let changed = guard_entry(&mut entry, NotFinite::Default).unwrap_or_default();
    if !changed.is_empty() {
        if let Ok(guarded) = serde_json::from_value(entry) {
            *effect = guarded;
        }
    }
    changed
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
    /// A single-picture node whose result can be limited by a `mask` and
    /// faded by `mix` against its input; then its inputs are [`MIXABLE_IN`]
    /// (an effect) or [`GATE_IN`] (a grade correction).
    pub mixable: bool,
}

/// Where a node can be placed: the effect lists it can sit in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Placement {
    /// A clip's.
    Clip,
    /// A title's.
    Title,
    /// An adjustment layer's.
    Adjustment,
}

impl NodeTypeDesc {
    /// The effect lists the node can be placed in. A node that reads other
    /// times than its frame's (`temporal`) reads a clip's source, so it is
    /// placed on a clip only; any other is placed on every effect list.
    pub fn placement(&self) -> &'static [Placement] {
        match self.temporal {
            Temporal::None => &[Placement::Clip, Placement::Title, Placement::Adjustment],
            Temporal::SourceNeighbours | Temporal::AnalysisDriven => &[Placement::Clip],
        }
    }
}

/// The effects browser's categories for built-in effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserCategory {
    Blur,
    Colour,
    Stylize,
    Keying,
    Distort,
    Light,
    Film,
}

/// Where the effects browser lists a built-in effect (#2888): every
/// effect a person puts on a clip is listed, and one that is not says
/// why.
#[derive(Debug, Clone, Copy)]
pub enum Browser {
    Listed {
        category: BrowserCategory,
        /// The heading the Category sort groups it under.
        grouping: &'static str,
        /// The line under its name; empty for none.
        description: &'static str,
        /// What adding it puts on the clip, when not its reset
        /// [`EffectType::sample`].
        start: Option<fn() -> BuiltinEffect>,
    },
    /// Not listed, and why: placed some other way than from the browser.
    Hidden(&'static str),
}

pub(crate) const fn listed(category: BrowserCategory, grouping: &'static str, description: &'static str) -> Browser {
    Browser::Listed { category, grouping, description, start: None }
}

pub(crate) const fn starting(category: BrowserCategory, grouping: &'static str, description: &'static str, start: fn() -> BuiltinEffect) -> Browser {
    Browser::Listed { category, grouping, description, start: Some(start) }
}

pub(crate) const fn hidden(why: &'static str) -> Browser {
    Browser::Hidden(why)
}

/// A built-in effect's description, tied to its variant.
#[derive(Debug, Clone, Copy)]
pub struct EffectType {
    pub desc: NodeTypeDesc,
    /// The variant's tag in a saved project (`"gaussian_blur"`).
    pub tag: &'static str,
    /// Where the effects browser lists it, or why it does not.
    pub browser: Browser,
    /// The effect as a reset leaves it: every parameter at its
    /// [`ParamDesc::default`].
    pub sample: fn() -> BuiltinEffect,
}

/// The saved tag of `effect`'s variant (`"gaussian_blur"`), if it is
/// described.
pub fn tag_of(effect: &BuiltinEffect) -> Option<&'static str> {
    static TAGS: std::sync::OnceLock<std::collections::HashMap<std::mem::Discriminant<BuiltinEffect>, &'static str>> = std::sync::OnceLock::new();
    TAGS.get_or_init(|| effect_types().map(|t| (std::mem::discriminant(&(t.sample)()), t.tag)).collect()).get(&std::mem::discriminant(effect)).copied()
}

/// Every built-in effect described so far.
pub fn effect_types() -> impl Iterator<Item = &'static EffectType> {
    colour::TYPES.iter().chain(geometry::TYPES).chain(film::TYPES).chain(blur::TYPES).chain(light::TYPES).chain(composite::TYPES)
}

/// The description of the effect saved under `tag`, if it is described.
pub fn effect_type(tag: &str) -> Option<&'static EffectType> {
    effect_types().find(|t| t.tag == tag)
}

/// Whether `effect` needs other times than its frame's (`Temporal`): the
/// source's neighbours, or a per-frame analysis of the clip. Read from
/// each registry type's declaration, so a new type that needs them is
/// covered by declaring it.
pub fn needs_other_times(effect: &BuiltinEffect) -> bool {
    static KINDS: std::sync::OnceLock<Vec<std::mem::Discriminant<BuiltinEffect>>> = std::sync::OnceLock::new();
    let kinds = KINDS.get_or_init(|| effect_types().filter(|t| t.desc.temporal != Temporal::None).map(|t| std::mem::discriminant(&(t.sample)())).collect());
    kinds.contains(&std::mem::discriminant(effect))
}

/// Whether `effect` needs the source's neighbouring frames
/// (`Temporal::SourceNeighbours`: temporal noise reduction, frame
/// blending), which run before the grade, outside a plan. The
/// analysis-driven types need other times too ([`needs_other_times`]),
/// but their per-frame data reaches a plan node as a value or as motion.
pub fn needs_source_neighbours(effect: &BuiltinEffect) -> bool {
    static KINDS: std::sync::OnceLock<Vec<std::mem::Discriminant<BuiltinEffect>>> = std::sync::OnceLock::new();
    let kinds = KINDS.get_or_init(|| effect_types().filter(|t| t.desc.temporal == Temporal::SourceNeighbours).map(|t| std::mem::discriminant(&(t.sample)())).collect());
    kinds.contains(&std::mem::discriminant(effect))
}

/// The order the source-boundary effects run in at a clip's source (M4
/// steps 6 and 7), by tag: the frame blend first, on the frames as
/// decoded, then the denoiser on the blended frame, so it compares the
/// picture shown with its neighbours. Every type that needs the source's
/// neighbours is in it.
pub const SOURCE_BOUNDARY_ORDER: [&str; 2] = ["speed_ramp", "temporal_denoise"];

/// Where `effect` runs among the source-boundary effects
/// ([`SOURCE_BOUNDARY_ORDER`]); `None` for any other.
pub fn source_boundary_rank(effect: &BuiltinEffect) -> Option<usize> {
    let tag = effect_types().find(|t| std::mem::discriminant(&(t.sample)()) == std::mem::discriminant(effect))?.tag;
    SOURCE_BOUNDARY_ORDER.iter().position(|t| *t == tag)
}

impl Scaling {
    /// Whether the effect scales it itself ([`BuiltinEffect::at_render_scale`]).
    pub fn by_the_effect(self) -> bool {
        matches!(self, Scaling::AtRenderScale | Scaling::AtRenderScaleAtLeastOne)
    }
}

/// `value` of the parameter `desc` describes, as the effect runs it on a
/// buffer `k` times the sequence's size (node-graph M2 step 13): what
/// [`BuiltinEffect::at_render_scale`] does to that field, read from the
/// registry. A pixel size is multiplied by `k`; a whole number of pixels
/// is rounded and kept at one or more where it was above zero; a block
/// is never below a pixel. Anything else, and any `k` that isn't a
/// positive number other than 1, leaves it as it is.
pub fn scaled_value(desc: &ParamDesc, value: f32, k: f32) -> f32 {
    if (k - 1.0).abs() < 1e-6 || !k.is_finite() || k <= 0.0 || !desc.scaling.by_the_effect() {
        return value;
    }
    match (desc.kind, desc.scaling) {
        (ParamKind::Whole, _) if value > 0.0 => (value * k).round().max(1.0),
        (ParamKind::Whole, _) => value,
        (_, Scaling::AtRenderScaleAtLeastOne) => (value * k).max(1.0),
        _ => value * k,
    }
}

#[cfg(test)]
mod source_boundary_order_tests {
    use super::*;

    /// Every effect that reads the source's neighbours has its place, the
    /// blend before the denoiser.
    #[test]
    fn every_source_boundary_effect_has_its_place_blend_first() {
        for t in effect_types().filter(|t| t.desc.temporal == Temporal::SourceNeighbours) {
            assert!(source_boundary_rank(&(t.sample)()).is_some(), "{} has no place at the source", t.tag);
        }
        let blend = source_boundary_rank(&(effect_type("speed_ramp").unwrap().sample)()).unwrap();
        let denoise = source_boundary_rank(&(effect_type("temporal_denoise").unwrap().sample)()).unwrap();
        assert!(blend < denoise, "the blend runs first");
        assert_eq!(source_boundary_rank(&BuiltinEffect::Invert { mix: 1.0 }), None);
    }
}
