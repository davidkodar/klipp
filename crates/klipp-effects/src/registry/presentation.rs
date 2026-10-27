//! How the inspector shows a parameter (M3 step 0a): the display facts the
//! hand-written rows hard-code today, kept apart from what the parameter
//! is ([`ParamDesc`]: its kind, range, default, keyframes). Two structs so
//! that no label, step or decimals change can alter a stored value, and
//! so that the semantic consumers (ops, the socket, the executor) never
//! carry display data.
//!
//! A parameter's [`Presentation`] is the entry its catalog's table gives
//! for its `(type id, key)`, or else the one [`Presentation::derived`]
//! makes from its description. The tables are data, one per family
//! (M3 step 0b for effects, 0c for grade nodes); nothing reads them until
//! the generated rows (step 5). UI-free: no egui types here.

use super::{EffectType, ParamDesc, ParamKind, Unit};
use klipp_grade_model::Encoding;

mod blur;
mod colour;
mod composite;
mod film;
mod geometry;
mod light;

/// How a parameter is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Presentation {
    /// The drag speed: the value's change per pixel dragged.
    pub step: f32,
    /// Decimals shown, 0 to 3.
    pub decimals: u8,
    pub suffix: Suffix,
    /// Shown and stored differ (the contrast pivot is stored linear and
    /// shown encoded). `None`: shown as stored.
    pub display: Option<DisplayMap>,
    /// A choice's labels, by index, or a switch's, off then on. They name
    /// the stored values for the eye and never rename them (the stored
    /// value is the index).
    pub labels: Option<&'static [&'static str]>,
    /// The row's label where it differs from [`ParamDesc::label`], kept
    /// only to hold today's wording.
    pub label: Option<&'static str>,
    pub widget: Widget,
    /// The sub-header the row sits under (a channel mixer's "Red Output",
    /// a wheel's triplet).
    pub group: Option<&'static str>,
    /// Where the row is shown among its type's, where that differs from
    /// the catalog's order; rows are shown by `order`, then by catalog
    /// order.
    pub order: u16,
    /// Shown only while this holds (a speed ramp's freeze point, while
    /// it freezes).
    pub visible_if: Option<Cond>,
    /// Editable only while this holds; shown dimmed otherwise.
    pub enabled_if: Option<Cond>,
    pub help: Option<&'static str>,
    /// What a reset writes where it must differ from
    /// [`ParamDesc::default`]. The list is kept to the reset audit's.
    pub reset: Option<f32>,
    /// A whole-number slider's range, where the catalog gives none (a
    /// radial blur's samples, a seed).
    pub soft_whole: Option<(i32, i32)>,
}

/// What follows the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suffix {
    /// The parameter's [`Unit`]'s: px, °, s, or nothing ([`Suffix::text`]).
    FromUnit,
    /// Text the unit doesn't give: `×`, `:1`, `stops`, `%`.
    Text(&'static str),
    None,
}

impl Suffix {
    /// The text shown after a value of `unit`.
    pub fn text(self, unit: Unit) -> &'static str {
        match self {
            Suffix::FromUnit => match unit {
                Unit::CanvasPx | Unit::Per1080 => "px",
                Unit::Degrees => "°",
                Unit::Seconds => "s",
                Unit::Ratio | Unit::Normalised | Unit::Count | Unit::None => "",
            },
            Suffix::Text(t) => t,
            Suffix::None => "",
        }
    }
}

/// A shown value made from a stored one and back: a pair of pure
/// functions, each the other's inverse over the parameter's slider range,
/// given the grade encoding in force (a contrast pivot is shown in it;
/// a map that doesn't depend on it ignores it).
#[derive(Debug, Clone, Copy)]
pub struct DisplayMap {
    pub to_shown: fn(f32, Encoding) -> f32,
    pub to_stored: fn(f32, Encoding) -> f32,
}

impl PartialEq for DisplayMap {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.to_shown, other.to_shown) && std::ptr::fn_addr_eq(self.to_stored, other.to_stored)
    }
}

impl DisplayMap {
    /// How far, relative to itself, a stored value may come back from
    /// shown: a log encoding's round trip in `f32` (ACEScc's `log2` and
    /// `exp2`) drifts by about 1e-6, a power curve's by two ulps; a shown
    /// value has three decimals at most, so this is far below a step.
    pub const ROUND_TRIP_REL: f32 = 1e-5;

    /// Every grade encoding a map is shown under.
    pub const ENCODINGS: [Encoding; 3] = {
        // A new encoding fails this match, so it's added here too.
        const fn every(e: Encoding) -> Encoding {
            match e {
                Encoding::Bt1886 | Encoding::AcesCct | Encoding::AcesCc => e,
            }
        }
        [every(Encoding::Bt1886), every(Encoding::AcesCct), every(Encoding::AcesCc)]
    };

    /// The first stored value in `range`, of `samples` across it, under
    /// any of [`Self::ENCODINGS`], that doesn't come back from shown within
    /// [`Self::ROUND_TRIP_REL`] of itself, if any.
    pub fn round_trip_fault(&self, range: (f32, f32), samples: usize) -> Option<(f32, f32)> {
        let n = samples.max(2);
        Self::ENCODINGS.into_iter().find_map(|enc| {
            (0..n).map(|i| range.0 + (range.1 - range.0) * i as f32 / (n - 1) as f32).find_map(|stored| {
                let back = (self.to_stored)((self.to_shown)(stored, enc), enc);
                let off = (back - stored).abs();
                (off.is_nan() || off > Self::ROUND_TRIP_REL * stored.abs().max(1e-3)).then_some((stored, back))
            })
        })
    }
}

/// The control a row draws. Two choices never imply a toggle: a
/// [`Widget::Toggle`] is always said, on a switch or a choice of two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widget {
    /// The label-drag scrub and its number.
    Scrub,
    /// A grade slider.
    Slider,
    Toggle,
    /// A choice's drop-down, labelled by [`Presentation::labels`] or by the
    /// catalog's choice names.
    Combo,
    ColourRgb,
    ColourRgba,
    /// A slider over a colour ramp.
    Gradient(Gradient),
    /// A slider over the hue circle.
    HueRainbow,
    /// Two values edited as a point (a corner pin's corner).
    Vec2,
    /// One part of a colour wheel; the wheel's parts share the group named.
    WheelPart(&'static str),
    /// No row; the value is kept as it is.
    Hidden,
    /// A value shown, not edited: what a tool of the node measured and
    /// writes (Colour Match's correction, written by Apply match, #1421).
    /// Owes opening without a write and its frame, not editing.
    Readout,
    /// A specialist editor, named (M3 step 0d). An empty name is one not
    /// named yet, which step 0d's coverage refuses.
    Custom(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gradient {
    Temperature,
    Tint,
}

/// A condition on a sibling parameter's stored value (never its animated
/// one, so a keyed switch doesn't make rows come and go).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cond {
    /// The parameter `key` holds `value`: 1 or 0 for a switch, the index
    /// for a choice.
    ParamIs { key: &'static str, value: f32 },
    /// The parameter `key` holds one of `values`: a choice's rows shown
    /// for some of its options (a Warp's shapes, #2758).
    ParamIn { key: &'static str, values: &'static [f32] },
}

impl Cond {
    /// The parameter the condition reads.
    pub fn key(&self) -> &'static str {
        match *self {
            Cond::ParamIs { key, .. } | Cond::ParamIn { key, .. } => key,
        }
    }

    /// Whether a stored `value` of it meets the condition.
    pub fn holds_for(&self, value: f32) -> bool {
        match *self {
            Cond::ParamIs { value: v, .. } => v == value,
            Cond::ParamIn { values, .. } => values.contains(&value),
        }
    }

    /// A value that meets it: what a check that shows the row sets.
    pub fn a_value_meeting_it(&self) -> f32 {
        match *self {
            Cond::ParamIs { value, .. } => value,
            Cond::ParamIn { values, .. } => values.first().copied().unwrap_or_default(),
        }
    }
}

/// How a node type is shown, beyond its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypePresentation {
    /// A line shown above the rows ("runs on the source").
    pub notice: Option<&'static str>,
    /// The adapter that draws what rows can't (a deflicker's analysis, a
    /// stabilizer's), by name (M3 step 6).
    pub extra_ui: Option<&'static str>,
}

/// One table entry: `pres` for parameter `key` of type `type_id`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamEntry {
    pub type_id: &'static str,
    pub key: &'static str,
    pub pres: Presentation,
}

/// One table entry: `pres` for type `type_id`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeEntry {
    pub type_id: &'static str,
    pub pres: TypePresentation,
}

/// The effects' entries, one table per family (M3 step 0b), transcribed
/// from today's hand-written rows.
pub static PARAMS: &[&[ParamEntry]] = &[colour::PARAMS, geometry::PARAMS, film::PARAMS, blur::PARAMS, light::PARAMS, composite::PARAMS];
pub static TYPES: &[&[TypeEntry]] = &[colour::TYPES, geometry::TYPES, film::TYPES, blur::TYPES, light::TYPES, composite::TYPES];

/// Every effect entry, family by family.
pub fn params() -> impl Iterator<Item = &'static ParamEntry> {
    PARAMS.iter().flat_map(|f| f.iter())
}

/// Every effect type entry, family by family.
pub fn types() -> impl Iterator<Item = &'static TypeEntry> {
    TYPES.iter().flat_map(|f| f.iter())
}

impl Presentation {
    /// A number dragged by `step` per pixel and shown to `decimals`: a
    /// row's start, the rest as today's default ([`Presentation::derived`]
    /// otherwise), changed with the builders below.
    pub const fn scrub(step: f32, decimals: u8) -> Self {
        Presentation {
            step,
            decimals,
            suffix: Suffix::FromUnit,
            display: None,
            labels: None,
            label: None,
            widget: Widget::Scrub,
            group: None,
            order: 0,
            visible_if: None,
            enabled_if: None,
            help: None,
            reset: None,
            soft_whole: None,
        }
    }

    /// A row that isn't a number: `widget`, with no step or decimals.
    pub const fn shown_as(widget: Widget) -> Self {
        Presentation { widget, ..Self::scrub(0.0, 0) }
    }

    /// No row; the value is kept.
    pub const fn hidden() -> Self {
        Self::shown_as(Widget::Hidden)
    }

    pub const fn widget(mut self, widget: Widget) -> Self {
        self.widget = widget;
        self
    }

    pub const fn display(mut self, display: DisplayMap) -> Self {
        self.display = Some(display);
        self
    }

    pub const fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    pub const fn labels(mut self, labels: &'static [&'static str]) -> Self {
        self.labels = Some(labels);
        self
    }

    pub const fn suffix(mut self, suffix: Suffix) -> Self {
        self.suffix = suffix;
        self
    }

    pub const fn group(mut self, group: &'static str) -> Self {
        self.group = Some(group);
        self
    }

    pub const fn order(mut self, order: u16) -> Self {
        self.order = order;
        self
    }

    pub const fn reset(mut self, reset: f32) -> Self {
        self.reset = Some(reset);
        self
    }

    pub const fn soft_whole(mut self, range: (i32, i32)) -> Self {
        self.soft_whole = Some(range);
        self
    }

    pub const fn help(mut self, help: &'static str) -> Self {
        self.help = Some(help);
        self
    }

    pub const fn visible_if(mut self, cond: Cond) -> Self {
        self.visible_if = Some(cond);
        self
    }

    pub const fn enabled_if(mut self, cond: Cond) -> Self {
        self.enabled_if = Some(cond);
        self
    }

    /// What a parameter is shown as when its table gives nothing: the
    /// widget its kind implies, the suffix its unit gives, a step of a
    /// two-hundredth of its slider's range.
    pub fn derived(p: &ParamDesc) -> Self {
        let (step, decimals) = match (p.kind, p.soft) {
            (ParamKind::Whole, _) => (1.0, 0),
            (ParamKind::Float, Some((lo, hi))) if hi > lo => ((hi - lo) / 200.0, 2),
            (ParamKind::Float, _) => (0.01, 2),
            _ => (0.0, 0),
        };
        let widget = match p.kind {
            ParamKind::Float | ParamKind::Whole => Widget::Scrub,
            ParamKind::Bool => Widget::Toggle,
            ParamKind::Choice(_) => Widget::Combo,
            ParamKind::Colour => Widget::ColourRgb,
            ParamKind::Other => Widget::Custom(""),
        };
        Presentation {
            step,
            decimals,
            suffix: Suffix::FromUnit,
            display: None,
            labels: None,
            label: None,
            widget,
            group: None,
            order: 0,
            visible_if: None,
            enabled_if: None,
            help: None,
            reset: None,
            soft_whole: None,
        }
    }

    /// `p`'s presentation: `table`'s entry for `(type_id, p.key)`, or the
    /// derived one.
    pub fn of(table: &[ParamEntry], type_id: &str, p: &ParamDesc) -> Self {
        Self::find(table.iter(), type_id, p)
    }

    /// [`Presentation::of`] over any entries.
    pub fn find<'a>(mut entries: impl Iterator<Item = &'a ParamEntry>, type_id: &str, p: &ParamDesc) -> Self {
        entries.find(|e| e.type_id == type_id && e.key == p.key).map_or_else(|| Self::derived(p), |e| e.pres)
    }
}

impl TypePresentation {
    /// `type_id`'s entry in `table`, or none.
    pub fn of(table: &[TypeEntry], type_id: &str) -> Self {
        table.iter().find(|e| e.type_id == type_id).map(|e| e.pres).unwrap_or_default()
    }
}

impl EffectType {
    /// How parameter `p` of this effect is shown.
    pub fn presentation(&self, p: &ParamDesc) -> Presentation {
        Presentation::find(params(), self.desc.id, p)
    }

    /// How this effect is shown, beyond its rows.
    pub fn type_presentation(&self) -> TypePresentation {
        types().find(|e| e.type_id == self.desc.id).map(|e| e.pres).unwrap_or_default()
    }
}

/// What is wrong with a catalog's tables, one line each: an entry naming
/// no parameter of `types` (each type's id and parameters), two entries for one parameter, decimals over
/// 3, a step that doesn't move a number, labels that don't match a
/// choice's count, a toggle for anything but a switch or a choice of two, a display map that
/// doesn't round-trip over the slider range, a type entry naming no type.
/// Both catalogs' coverage tests hold their tables to it.
pub fn problems(types: &[(&str, &[ParamDesc])], params: &[ParamEntry], type_entries: &[TypeEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, e) in params.iter().enumerate() {
        let Some(p) = types.iter().find(|(id, _)| *id == e.type_id).and_then(|(_, ps)| ps.iter().find(|p| p.key == e.key)) else {
            out.push(format!("{}/{}: names no parameter", e.type_id, e.key));
            continue;
        };
        if params[..i].iter().any(|o| o.type_id == e.type_id && o.key == e.key) {
            out.push(format!("{}/{}: two entries", e.type_id, e.key));
        }
        out.extend(param_problems(e.type_id, p, &e.pres));
    }
    for e in type_entries {
        if !types.iter().any(|(id, _)| *id == e.type_id) {
            out.push(format!("{}: names no type", e.type_id));
        }
    }
    out
}

/// What is wrong with `pres` for `p`, one line each (see [`problems`]).
pub fn param_problems(type_id: &str, p: &ParamDesc, pres: &Presentation) -> Vec<String> {
    let at = format!("{type_id}/{}", p.key);
    let mut out = Vec::new();
    if pres.decimals > 3 {
        out.push(format!("{at}: {} decimals", pres.decimals));
    }
    let numeric = matches!(p.kind, ParamKind::Float | ParamKind::Whole);
    // A readout isn't dragged, so it has no step (#1421).
    if numeric && !matches!(pres.widget, Widget::Hidden | Widget::Readout | Widget::Custom(_)) && !(pres.step.is_finite() && pres.step > 0.0) {
        out.push(format!("{at}: step {} doesn't move it", pres.step));
    }
    match (p.kind, pres.labels) {
        (ParamKind::Choice(names), Some(labels)) if labels.len() != names.len() => out.push(format!("{at}: {} labels for {} choices", labels.len(), names.len())),
        // A switch's two states, off then on, named.
        (ParamKind::Bool, Some(labels)) if labels.len() != 2 => out.push(format!("{at}: {} labels for a switch", labels.len())),
        (ParamKind::Choice(_) | ParamKind::Bool, _) | (_, None) => {}
        (_, Some(_)) => out.push(format!("{at}: labels on a parameter that isn't a choice or a switch")),
    }
    // A toggle is said, never implied: a switch, or a choice of two
    // (off and on) shown as one.
    let two = matches!(p.kind, ParamKind::Choice(c) if c.len() == 2);
    if pres.widget == Widget::Toggle && p.kind != ParamKind::Bool && !two {
        out.push(format!("{at}: a toggle for a {:?}", p.kind));
    }
    if let (Some(map), Some(range)) = (pres.display, p.soft) {
        if let Some((stored, back)) = map.round_trip_fault(range, 1001) {
            out.push(format!("{at}: shown and back, {stored} comes back {back}"));
        }
    }
    out
}
