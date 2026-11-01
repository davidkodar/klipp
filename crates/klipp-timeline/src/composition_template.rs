//! Composition templates (#225, #2757): a composition (a
//! precomposition and any sequences it nests) with typed parameters bound
//! to its properties, saved to start from in any project. The renderer
//! needs no template path of its own: an instance's values are put on the
//! ordinary composition when the timeline is flattened.
//!
//! The file carries its composition (format 2), so a template inserts
//! into any project: the sequences it needs are copied in when the
//! project lacks them. A file of format 1 only named its composition and
//! is refused, as is one of a newer format than this build writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use klipp_core::TemplateValue;

use crate::Sequence;

/// File extension for composition templates.
pub const EXTENSION: &str = "klipptemplate";
/// The most a template file may be (#2758): over any made here (a recipe
/// is a few kilobytes; a composition carries whole sequences, titles with
/// their runs, animators and effect stacks, and its format variants), so
/// a file past it is not one, and isn't read.
pub const MAX_FILE_BYTES: u64 = 16 << 20;
/// The most of each a template holds; see [`CompositionTemplate::validate`].
pub const MAX_PARAMETERS: usize = 64;
pub const MAX_BINDINGS: usize = 256;
pub const MAX_SLOTS: usize = 16;
pub const MAX_SEQUENCES: usize = 64;
/// A recipe's graph: as many ops as one graph batch takes.
pub const MAX_RECIPE_OPS: usize = 64;
pub const MAX_RECIPE_CHECKS: usize = 32;
/// The most keys a recipe's `keys` op puts on one parameter (#2886).
pub const MAX_RECIPE_KEYS: usize = 256;
/// The longest name, label or property, bytes.
pub const MAX_TEXT: usize = 256;
/// The most steps an audio chain recipe adds (#2886).
pub const MAX_AUDIO_STEPS: usize = 16;
/// The most steps a clips recipe makes, and keys one step sets (#2886).
pub const MAX_CLIP_STEPS: usize = 32;
pub const MAX_CLIP_KEYS: usize = 64;
/// The format this build writes and reads: 2 carries the composition.
pub const FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumberRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// A parameter declaration and its default value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateParameter {
    pub label: String,
    pub default: TemplateValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<NumberRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

impl TemplateParameter {
    pub fn validate(&self, value: &TemplateValue) -> Result<(), String> {
        let same_kind = matches!(
            (&self.default, value),
            (TemplateValue::Text(_), TemplateValue::Text(_))
                | (TemplateValue::Color(_), TemplateValue::Color(_))
                | (TemplateValue::Number(_), TemplateValue::Number(_))
                | (TemplateValue::Bool(_), TemplateValue::Bool(_))
                | (TemplateValue::Enum(_), TemplateValue::Enum(_))
        );
        if !same_kind {
            return Err(format!("value for '{}' has the wrong type", self.label));
        }
        if let TemplateValue::Number(number) = value {
            // NaN and infinity have no JSON form: serde writes `null`,
            // and the saved template would never load again.
            if !number.is_finite() {
                return Err(format!("value for '{}' is not a number", self.label));
            }
            if let Some(range) = &self.range {
                if range.min.is_some_and(|min| *number < min)
                    || range.max.is_some_and(|max| *number > max)
                {
                    return Err(format!("value for '{}' is outside its range", self.label));
                }
            }
        }
        if let TemplateValue::Enum(choice) = value {
            if !self.choices.is_empty() && !self.choices.iter().any(|item| item == choice) {
                return Err(format!("unknown choice '{choice}' for '{}'", self.label));
            }
        }
        Ok(())
    }
}

/// A binding from an exposed parameter to a property in the ordinary
/// composition.  The property path is intentionally opaque to this layer;
/// the document resolver owns its interpretation and can evolve it without
/// changing the on-disk template registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateBinding {
    pub parameter: String,
    pub property: String,
    /// A control's link (macro, #2757): the number parameter mapped from
    /// its range onto `to` along `curve`, one of the several properties it
    /// may drive. `None` for a plain binding, which sets the property to
    /// the parameter's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkMap>,
    /// What of the control's value the property takes (#2758): `None`
    /// the value itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<Take>,
}

/// What a binding takes of its control's value (#2758).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Take {
    /// A colour's hue, degrees, as a qualifier reads the colour picked
    /// on the picture: what keys a colour by the colour a person picks.
    Hue,
}

/// `value` as a binding taking `take` of it sets its property (#2758):
/// the value itself, or a colour's hue in degrees, read as the
/// qualifier's picker reads a picked colour. The one place a binding's
/// value is taken apart.
pub fn taken(value: &TemplateValue, take: Option<Take>) -> Result<TemplateValue, String> {
    match (take, value) {
        (None, v) => Ok(v.clone()),
        (Some(Take::Hue), TemplateValue::Color(c)) => {
            // Picked as sRGB; the qualifier reads the picture's linear light.
            let linear = [c[0], c[1], c[2]].map(|v| klipp_color::transfer::srgb_to_linear(v.clamp(0.0, 1.0)));
            Ok(TemplateValue::Number(f64::from(klipp_effects::cpu_grade::picture_hsl(linear).h * 360.0)))
        }
        (Some(Take::Hue), _) => Err("a hue is taken from a colour".into()),
    }
}

/// How a control drives one property (#2757).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkMap {
    pub to: [f64; 2],
    #[serde(default)]
    pub curve: klipp_core::LinkCurve,
    /// What a recipe's link is measured against where it is placed
    /// (#2758): a fraction of the frame's width or height made pixels for
    /// the sequence, so it places the same at any size; or seconds into
    /// the clip made its source seconds, so a time stays on its frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per: Option<Per>,
}

/// What a recipe's link is measured against (#2758).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Per {
    /// The frame's width.
    Width,
    /// The frame's height.
    Height,
    /// Seconds into the clip, made its media's own seconds.
    Source,
}

/// Where a recipe is placed (#2758), what its links are measured against:
/// the sequence's `frame`, and the clip, `length` seconds long, whose
/// source time `source_at` seconds into it is.
pub struct Placing<'a> {
    pub frame: klipp_core::Resolution,
    pub length: f64,
    pub source_at: &'a dyn Fn(f64) -> f64,
}

/// A time as itself.
fn same_time(t: f64) -> f64 {
    t
}

impl Placing<'static> {
    /// On a frame alone: a clip of any length whose source time is its own.
    pub fn on_frame(frame: klipp_core::Resolution) -> Self {
        Placing { frame, length: f64::INFINITY, source_at: &same_time }
    }
}

impl Placing<'_> {
    /// A link's ends `to`, made what it is measured `per`.
    fn measured(&self, per: Option<Per>, to: [f64; 2]) -> [f64; 2] {
        match per {
            Some(Per::Width) => to.map(|v| v * self.frame.width as f64),
            Some(Per::Height) => to.map(|v| v * self.frame.height as f64),
            Some(Per::Source) => to.map(|v| (self.source_at)(v)),
            None => to,
        }
    }
}

/// The instance a placed `template` starts as (#2757): plain bindings by
/// parameter, each control's links with its range, and each control's
/// default, so the resolver, which never sees the template, knows where
/// the control stands.
pub fn instance_for(template: &CompositionTemplate) -> klipp_core::TemplateInstance {
    let mut instance = klipp_core::TemplateInstance { template_id: template.id, bindings: BTreeMap::new(), overrides: BTreeMap::new(), media_slots: BTreeMap::new(), links: Vec::new() };
    for b in &template.bindings {
        match (&b.link, template.parameters.get(&b.parameter)) {
            (Some(map), Some(p)) => {
                let from = [p.range.as_ref().and_then(|r| r.min).unwrap_or(0.0), p.range.as_ref().and_then(|r| r.max).unwrap_or(1.0)];
                instance.links.push(klipp_core::TemplateLink { parameter: b.parameter.clone(), property: b.property.clone(), from, to: map.to, curve: map.curve, offset: 0.0, linked: true, frozen: 0.0 });
                instance.overrides.entry(b.parameter.clone()).or_insert_with(|| p.default.clone());
            }
            _ => {
                instance.bindings.insert(b.parameter.clone(), b.property.clone());
            }
        }
    }
    instance
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionTemplate {
    pub format_version: u32,
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub category: String,
    /// The sequence/precomposition that supplies the actual layers.
    pub composition_id: Uuid,
    #[serde(default)]
    pub parameters: BTreeMap<String, TemplateParameter>,
    #[serde(default)]
    pub bindings: Vec<TemplateBinding>,
    #[serde(default)]
    pub written_by: String,
    /// The composition and every sequence it nests (format 2), so the
    /// template inserts into any project.
    #[serde(default)]
    pub sequences: Vec<Sequence>,
    /// Its slots (#2757), by name: what the user fills in when the
    /// template is placed or later, each bound like a parameter (a media
    /// slot to `clip:<n>.media`); until filled it shows a placeholder.
    /// The same slots a Composite recipe declares (#2758).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<String, TemplateSlot>,
    /// A composition placed as a clip, or a recipe built into a clip's
    /// picture (#2758).
    #[serde(default, skip_serializing_if = "TemplateKind::is_composition")]
    pub kind: TemplateKind,
    /// A recipe's graph (#2758): what placing it adds to the clip's
    /// picture. `None` for a composition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<RecipeGraph>,
    /// An audio chain recipe's steps (#2886): what placing it adds to a
    /// clip's sound. `None` for any other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioChain>,
    /// A clips recipe's steps (#2886): what placing it does to the clip it
    /// is placed on and the clips found from it. `None` for any other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clips: Option<ClipRecipe>,
    /// The composition laid out for other frames (#2757): `tall` (9:16
    /// and other portrait frames) and `square`, each the id of one of
    /// `sequences`, with the same layers in the same order, so one set of
    /// bindings fits all. A frame without its own is given the main one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub formats: BTreeMap<String, Uuid>,
}

/// What a template file holds (#2758): a composition, placed as a clip
/// of its own, or a recipe, a node group with slots and controls built
/// into a clip's picture. One file format, one store, one listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    #[default]
    Composition,
    Recipe,
    /// A chain of Klipp's own audio effects added to a clip's sound
    /// (#2886).
    AudioChain,
    /// Edits of the clip it is placed on and clips found from it: a matte,
    /// keys (#2886).
    Clips,
}

/// A clips recipe (#2886): how each clip slot is found from the clip the
/// recipe is placed on when nothing fills it, and the steps, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipRecipe {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub find: BTreeMap<String, ClipFind>,
    pub steps: Vec<ClipStep>,
}

/// A clip found from the one a recipe is placed on (#2886).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipFind {
    /// The clip after it on its track.
    NextOnTrack,
    /// The first clip on a track above it that overlaps it in time, an
    /// adjustment layer passed over.
    Above,
}

impl ClipFind {
    /// What a clip needs for this to be found, as a refusal says it.
    pub fn needs(self) -> &'static str {
        match self {
            ClipFind::NextOnTrack => "a next clip on the same track",
            ClipFind::Above => "a clip on a higher track at the same time",
        }
    }
}

/// One step of a [`ClipRecipe`] (#2886): on `clip` (`placed`, the clip it
/// is placed on, or a clip slot), a matte or keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipStep {
    pub clip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<MatteStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<KeysStep>,
}

/// The clip seen through another, a slot's (#2886), as the `matte` op
/// sets it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatteStep {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub feather_px: f32,
    #[serde(default)]
    pub choke_px: f32,
}

/// Keys on one of a clip's own animated properties (#2886), by its name
/// in [`CLIP_KEY_PARAMS`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeysStep {
    pub param: String,
    pub keys: Vec<ClipKey>,
}

/// One key of a [`KeysStep`]: when, seconds from the clip's start or
/// before its end, and the value, plain or in the frame's widths or
/// heights (pixels of the sequence).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipKey {
    pub at: KeyTime,
    pub value: KeyValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyTime {
    Secs(f64),
    FromEnd { from_end: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyValue {
    Number(f32),
    FrameWidths { frame_widths: f32 },
    FrameHeights { frame_heights: f32 },
}

impl KeyTime {
    /// Seconds into a clip `secs` long.
    pub fn in_clip(self, secs: f64) -> f64 {
        match self {
            KeyTime::Secs(s) => s,
            KeyTime::FromEnd { from_end } => (secs - from_end).max(0.0),
        }
    }

    fn finite(self) -> bool {
        match self {
            KeyTime::Secs(s) | KeyTime::FromEnd { from_end: s } => s.is_finite() && s >= 0.0,
        }
    }
}

impl KeyValue {
    /// The value in a `width` × `height` frame.
    pub fn in_frame(self, width: u32, height: u32) -> f32 {
        match self {
            KeyValue::Number(v) => v,
            KeyValue::FrameWidths { frame_widths } => frame_widths * width as f32,
            KeyValue::FrameHeights { frame_heights } => frame_heights * height as f32,
        }
    }

    fn finite(self) -> bool {
        match self {
            KeyValue::Number(v) | KeyValue::FrameWidths { frame_widths: v } | KeyValue::FrameHeights { frame_heights: v } => v.is_finite(),
        }
    }
}

/// The clip properties a clips recipe's keys may set (#2886), by name.
pub const CLIP_KEY_PARAMS: [(&str, klipp_core::VideoParam); 8] = [
    ("opacity", klipp_core::VideoParam::Opacity),
    ("position_x", klipp_core::VideoParam::PositionX),
    ("position_y", klipp_core::VideoParam::PositionY),
    ("scale", klipp_core::VideoParam::Scale),
    ("scale_y", klipp_core::VideoParam::ScaleY),
    ("rotation", klipp_core::VideoParam::Rotation),
    ("matte_feather", klipp_core::VideoParam::MatteFeather),
    ("matte_choke", klipp_core::VideoParam::MatteChoke),
];

/// The property a clips recipe's keys name `name` sets.
pub fn clip_key_param(name: &str) -> Option<klipp_core::VideoParam> {
    CLIP_KEY_PARAMS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}

/// `steps.<i>.keys.<j>.value|at` (#2886): key `j` of a clips recipe's
/// step `i`, its value or its time.
pub fn clip_key_binding(property: &str) -> Option<(usize, usize, &str)> {
    let rest = property.strip_prefix("steps.")?;
    let (i, rest) = rest.split_once(".keys.")?;
    let (j, field) = rest.split_once('.')?;
    matches!(field, "value" | "at").then_some((i.parse().ok()?, j.parse().ok()?, field))
}

/// An audio chain recipe's steps, in the order they are added (#2886).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioChain {
    pub steps: Vec<AudioStep>,
}

/// One step of an [`AudioChain`]: its name, the built-in audio effect it
/// adds by its type id, the settings it gives it (the rest the effect's
/// own), its wet/dry mix, and what a reply says it did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioStep {
    pub name: String,
    pub effect: String,
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    #[serde(default = "full_mix")]
    pub wet_dry: f32,
    #[serde(default)]
    pub says: String,
}

fn full_mix() -> f32 {
    1.0
}

/// `step:<name>.<param>` (#2886): setting `param` of an audio chain
/// recipe's step `name`.
pub fn step_binding(property: &str) -> Option<(&str, &str)> {
    let (step, param) = property.strip_prefix("step:")?.split_once('.')?;
    (!step.is_empty() && !param.is_empty()).then_some((step, param))
}

impl TemplateKind {
    pub fn is_composition(&self) -> bool {
        *self == TemplateKind::Composition
    }
}

/// A recipe's graph (#2758): the graph ops a `klipp_graph` batch takes
/// (add, add_beside, connect, set...), with each path either
/// `{"anchor": <name>}`, a node of the clip's picture named for what it is
/// ([`ANCHORS`]), or `{"new": k}`, the node op `k` added. A binding
/// `node:<k>.<param>` sets parameter `param` of the node op `k` adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeGraph {
    pub ops: Vec<serde_json::Value>,
    /// What the recipe harness measures of it (#2758), on its test shot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<RecipeCheck>,
    /// The controls its checks run with, by name, where the test shot
    /// needs other values than a real picture's defaults (a sky's
    /// brightness against the shot's plain patch).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub check_with: BTreeMap<String, f64>,
}

/// One measured check of a recipe (#2758), as data: the frame the recipe
/// harness draws with the recipe placed on its test shot (a grey frame,
/// a green patch over its middle fifth to four fifths, `[0.3, 0.7]` both
/// ways; a media slot holds a red picture, a colour slot the patch's
/// green), read at `at` (fractions of the frame) in `channel`, from 0 to
/// 1, lies within `[min, max]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeCheck {
    pub at: [f32; 2],
    #[serde(default)]
    pub channel: klipp_core::ViewChannel,
    #[serde(default)]
    pub min: f32,
    #[serde(default = "one")]
    pub max: f32,
    /// What it shows, said when it fails.
    pub says: String,
}

fn one() -> f32 {
    1.0
}

/// The nodes of a clip's picture a recipe attaches to (#2758): its
/// source, its grade, its display step, and the end of its chain (what
/// feeds the placement, or the output); and the clip's placement (#2886),
/// which only a `keys` op names, to key its position, scale or rotation.
pub const ANCHORS: [&str; 5] = ["source", "grade", "display", "end", "placement"];

/// `keys:<op>#<i>.<field>` (#2886): the time (`time_secs`), value
/// (`value`) or ease into the next key (`ease`) of key `i` of the recipe's
/// `keys` op `op`, whatever it keys (a node it adds, or the clip's
/// placement).
pub fn keys_op_binding(property: &str) -> Option<(usize, usize, &str)> {
    let (op, key) = property.strip_prefix("keys:")?.split_once('#')?;
    let (i, field) = key.split_once('.')?;
    matches!(field, "time_secs" | "value" | "ease").then_some((op.parse().ok()?, i.parse().ok()?, field))
}

/// The fields of a graph op that are paths: a recipe's anchors and new
/// nodes are resolved there.
const PATH_FIELDS: [&str; 5] = ["after", "at", "from", "to", "path"];

/// `node:<k>.<param>` (#2758): the op `k` and its parameter.
pub fn node_binding(property: &str) -> Option<(usize, &str)> {
    let (k, param) = property.strip_prefix("node:")?.split_once('.')?;
    let k = k.parse().ok()?;
    (!param.is_empty()).then_some((k, param))
}

/// `node:<k>.<param>/<pointer>` (#2886): a value inside the parameter
/// `param` of the node op `k` adds, by JSON pointer: one number of a
/// grade mask's rectangle (`node:3.masks/masks/0/shape/Rectangle/center_x`).
/// The parameter and the pointer; `None` for a parameter bound whole.
pub fn nested_binding(param: &str) -> Option<(&str, String)> {
    let (head, rest) = param.split_once('/')?;
    (!head.is_empty() && !rest.is_empty()).then(|| (head, format!("/{rest}")))
}

/// `node:<k>.<param>#<i>.<field>` (#2886): the time (`time_secs`) or
/// the value (`value`) of key `i` a recipe's `keys` op puts on parameter
/// `param` of the node op `k` adds; `None` for another property.
pub fn key_binding(property: &str) -> Option<(usize, &str, usize, &str)> {
    let (k, rest) = node_binding(property)?;
    let (param, key) = rest.split_once('#')?;
    let (i, field) = key.split_once('.')?;
    (!param.is_empty() && matches!(field, "time_secs" | "value")).then_some((k, param, i.parse().ok()?, field))
}

/// Whether recipe op `op` is the `keys` op on parameter `param` of the
/// node op `k` adds.
fn keys_on(op: &serde_json::Value, k: usize, param: &str) -> bool {
    let new = serde_json::json!({ "new": k });
    let path = &op["path"];
    op["op"] == "keys" && op["key"] == param && (*path == new || path["nodes"].as_array().is_some_and(|n| n.len() == 1 && n[0] == new))
}

/// `node:<k>.<param>~<field>` (#2758): a field of the first behaviour a
/// recipe's `modify` op puts on parameter `param` of the node op `k`
/// adds (a wiggle's `amplitude`); `None` for another property.
pub fn behaviour_binding(property: &str) -> Option<(usize, &str, &str)> {
    let (k, rest) = node_binding(property)?;
    let (param, field) = rest.split_once('~')?;
    (!param.is_empty() && !field.is_empty()).then_some((k, param, field))
}

/// Whether recipe op `op` is the `modify` op on parameter `param` of the
/// node op `k` adds: its path that new node, before or after the path is
/// rooted.
fn modifies(op: &serde_json::Value, k: usize, param: &str) -> bool {
    let new = serde_json::json!({ "new": k });
    let path = &op["path"];
    op["op"] == "modify" && op["key"] == param && (*path == new || path["nodes"].as_array().is_some_and(|n| n.len() == 1 && n[0] == new))
}

/// The anchors of `clip`'s picture (#2758), by name: the node ids down
/// to each.
pub fn recipe_anchors(clip: &crate::Clip) -> Result<BTreeMap<&'static str, Uuid>, String> {
    use klipp_graph_format::lifted::types as lt;
    let lifted = clip.picture_v2().map_err(|e| e.to_string())?;
    let graph = &lifted.picture.graph;
    let of = |t: &str| graph.nodes.iter().find(|n| n.type_id.as_str() == t).map(|n| n.id);
    let feeding = |node: Uuid| graph.edges.iter().filter(|e| e.to.0 == node).min_by_key(|e| e.to.1.as_str() != "in").map(|e| e.from.0);
    let mut end = feeding(graph.output);
    if let Some(n) = end.filter(|n| graph.nodes.iter().any(|x| x.id == *n && x.type_id.as_str() == lt::PLACEMENT)) {
        end = feeding(n);
    }
    let mut anchors = BTreeMap::new();
    for (name, id) in [("source", of(lt::SOURCE)), ("grade", of(lt::GROUP)), ("display", of(lt::SCENE_TO_DISPLAY)), ("end", end)] {
        if let Some(id) = id {
            anchors.insert(name, id);
        }
    }
    // The clip's placement, by its node or the id it is always found by.
    let placement = klipp_graph_lift::clip_properties::placement_node(&lifted.picture).map_or_else(|| klipp_graph_lift::clip_properties::placement_id(clip.id.0), |n| n.id);
    anchors.insert("placement", placement);
    Ok(anchors)
}

/// A recipe of the nodes `nodes` of `clip`'s picture (#2757 step 6), each
/// as it is, its whole stored entry carried: the ones on the chain added
/// in their order after the clip's source, grade or display step that
/// comes before them (else the chain's end), the ones beside it beside the
/// source; the wires among them kept, and the ones from the source, grade
/// or display step kept as those anchors; a node's mix, mask channel and
/// switch, and each behaviour on a parameter (a modify op; its keys are
/// not kept). `controls` publish numbers of them, each drive's target
/// `node:<id>.<param>`. Refused, named: a node the picture's structure
/// needs, a node of a kind a recipe can't hold yet, chain nodes not next
/// to each other, and a wire to or from a node left out.
pub fn recipe_from_nodes(clip: &crate::Clip, nodes: &[uuid::Uuid], name: &str, category: Option<String>, controls: &[ControlSpec]) -> Result<CompositionTemplate, String> {
    use klipp_graph_format::lifted::types as lt;
    use klipp_graph_format::types;
    if nodes.is_empty() {
        return Err("name the nodes to save".into());
    }
    let lifted = clip.picture_v2().map_err(|e| e.to_string())?;
    let graph = &lifted.picture.graph;
    let node = |id: &uuid::Uuid| graph.nodes.iter().find(|n| n.id == *id);
    let label = |id: &uuid::Uuid| node(id).map(|n| if n.label.is_empty() { klipp_graph_lift::effect_params::tag(n).unwrap_or(n.type_id.as_str()).to_string() } else { n.label.clone() }).unwrap_or_else(|| id.to_string());
    let mut saved: Vec<uuid::Uuid> = Vec::new();
    for id in nodes {
        let n = node(id).ok_or_else(|| format!("{id} is not a node at the top of the clip's picture"))?;
        if [lt::SOURCE, lt::GROUP, lt::SCENE_TO_DISPLAY, lt::PLACEMENT, lt::OUTPUT].contains(&n.type_id.as_str()) || n.id == graph.output {
            return Err(format!("{} is part of every clip's picture, not a node to save", label(id)));
        }
        if !saved.contains(id) {
            saved.push(*id);
        }
    }
    // The chain, source to output, as the renderer walks it.
    let mut chain = vec![graph.output];
    while let Some(e) = graph.edges.iter().find(|e| e.to == (*chain.last().unwrap(), "in".into())) {
        if chain.len() > graph.nodes.len() {
            return Err("the picture's chain loops".into());
        }
        chain.push(e.from.0);
    }
    chain.reverse();
    let on_chain: Vec<uuid::Uuid> = chain.iter().copied().filter(|id| saved.contains(id)).collect();
    let at: Vec<usize> = on_chain.iter().map(|id| chain.iter().position(|c| c == id).unwrap()).collect();
    if at.windows(2).any(|w| w[1] != w[0] + 1) {
        return Err("the chain's nodes saved together have to follow each other on it".into());
    }
    let anchors = recipe_anchors(clip)?;
    let anchor_of = |id: uuid::Uuid| anchors.iter().find(|(n, a)| **a == id && **n != "end").map(|(n, _)| *n);
    let order: Vec<uuid::Uuid> = on_chain.iter().copied().chain(saved.iter().copied().filter(|id| !on_chain.contains(id))).collect();
    let k = |id: &uuid::Uuid| order.iter().position(|o| o == id);
    let mut ops: Vec<serde_json::Value> = Vec::new();
    for (i, id) in order.iter().enumerate() {
        let n = node(id).expect("checked above");
        let (type_id, entry) = match n.type_id.as_str() {
            types::LEGACY_EFFECT => {
                let tag = klipp_graph_lift::effect_params::tag(n).ok_or_else(|| format!("{} has no effect type", label(id)))?;
                let t = klipp_effects::registry::effect_type(tag).ok_or_else(|| format!("{tag} is not an effect this Klipp has"))?;
                (t.desc.id.to_string(), klipp_graph_lift::effect_params::entry_of(n)?)
            }
            types::LEGACY_NODE => {
                // A grade node's stored entry: its kind, label and settings.
                let entry = n.legacy.as_ref().map(|l| l.payload.clone()).ok_or_else(|| format!("{} has no entry", label(id)))?;
                let kind = serde_json::from_value(entry["kind"].clone()).map_err(|e| format!("{}: {e}", label(id)))?;
                let t = klipp_nodes::registry::node_type(kind).ok_or_else(|| format!("{} is not a node a recipe can add", label(id)))?;
                (t.desc.id.to_string(), entry)
            }
            other => return Err(format!("{} is a {other} node, which a recipe can't hold yet", label(id))),
        };
        let mut op = if on_chain.contains(id) {
            let after = if i == 0 { chain.get(at[0].wrapping_sub(1)).and_then(|p| anchor_of(*p)).map_or(serde_json::json!({ "anchor": "end" }), |a| serde_json::json!({ "anchor": a })) } else { serde_json::json!({ "new": i - 1 }) };
            serde_json::json!({ "op": "add", "after": after, "type": type_id, "entry": entry })
        } else {
            serde_json::json!({ "op": "add_beside", "at": { "anchor": "source" }, "type": type_id, "entry": entry })
        };
        if !n.label.is_empty() {
            op["label"] = serde_json::json!(n.label);
        }
        ops.push(op);
    }
    // The wires: into a saved node from another, or from an anchor; a
    // chain node's own input from the node before it is its place.
    for e in &graph.edges {
        let (from, to) = (e.from.0, e.to.0);
        let (from_saved, to_saved) = (saved.contains(&from), saved.contains(&to));
        let chain_link = e.to.1.as_str() == "in" && chain.windows(2).any(|w| w[0] == from && w[1] == to);
        if chain_link && (to_saved || from_saved) {
            continue;
        }
        if !to_saved && from_saved {
            return Err(format!("{} reads {}, which isn't saved: include it, or leave {} out", label(&to), label(&from), label(&from)));
        }
        if !to_saved {
            continue;
        }
        let source = match (k(&from), anchor_of(from)) {
            (Some(i), _) => serde_json::json!({ "new": i }),
            (None, Some(a)) => serde_json::json!({ "anchor": a }),
            (None, None) => return Err(format!("{} reads {}, which isn't saved: include it", label(&to), label(&from))),
        };
        ops.push(serde_json::json!({ "op": "connect", "from": source, "from_port": e.from.1.as_str(), "to": { "new": k(&to).unwrap() }, "to_port": e.to.1.as_str() }));
    }
    // Each node's mix, mask channel, switch and behaviours.
    for (i, id) in order.iter().enumerate() {
        let n = node(id).expect("checked above");
        if let Some(mix) = n.mix {
            ops.push(serde_json::json!({ "op": "mix", "path": { "new": i }, "mix": mix }));
        }
        if let Some(klipp_graph_format::ParamValue::Text(channel)) = n.params.get(&klipp_graph_format::ParamKey::from("mask.channel")) {
            ops.push(serde_json::json!({ "op": "mask_channel", "path": { "new": i }, "channel": channel }));
        }
        if !n.enabled {
            ops.push(serde_json::json!({ "op": "enable", "path": { "new": i }, "enabled": false }));
        }
        for (key, anim) in &n.anim {
            if let Some(stack) = anim.modifiers.as_ref().and_then(|m| m.get("modifiers")).filter(|m| m.as_array().is_some_and(|a| !a.is_empty())) {
                ops.push(serde_json::json!({ "op": "modify", "path": { "new": i }, "key": key.as_str(), "modifiers": stack }));
            }
        }
    }
    let mut t = CompositionTemplate::new(name.trim(), Uuid::nil());
    t.kind = TemplateKind::Recipe;
    t.category = category.unwrap_or_default();
    // Each control a number across its range, driving its targets; at
    // rest, where its first target is now, so the recipe places as the
    // nodes were saved.
    for c in controls {
        let control = control_name(&c.label);
        let (min, max) = (c.min.unwrap_or(0.0), c.max.unwrap_or(1.0));
        let at_rest = c.default.or_else(|| {
            let d = c.drives.first()?;
            let (target, param) = d.target.strip_prefix("node:")?.split_once('.')?;
            let now = node_number(node(&target.parse().ok()?)?, param)?;
            let span = d.to[1] - d.to[0];
            (span.abs() > f64::EPSILON).then(|| (min + (now - d.to[0]) / span * (max - min)).clamp(min.min(max), min.max(max)))
        });
        let parameter: TemplateParameter = serde_json::from_value(serde_json::json!({ "label": c.label, "default": { "type": "Number", "value": at_rest.unwrap_or(min) }, "range": { "min": min, "max": max } })).map_err(|e| e.to_string())?;
        t.parameters.insert(control.clone(), parameter);
        for d in &c.drives {
            let (target, param) = d.target.strip_prefix("node:").and_then(|r| r.split_once('.')).ok_or_else(|| format!("control \"{}\" drives node:<id>.<param>, not '{}'", c.label, d.target))?;
            let id: uuid::Uuid = target.parse().map_err(|_| format!("control \"{}\": {target} is not a node id", c.label))?;
            let i = k(&id).ok_or_else(|| format!("control \"{}\" drives {}, which isn't saved", c.label, label(&id)))?;
            t.bindings.push(TemplateBinding { parameter: control.clone(), property: format!("node:{i}.{param}"), link: Some(LinkMap { to: d.to, curve: d.curve, per: None }), take: None });
        }
    }
    t.graph = Some(serde_json::from_value(serde_json::json!({ "ops": ops })).map_err(|e| e.to_string())?);
    t.validate()?;
    Ok(t)
}

/// The number parameter `key` of node `n` holds now (#2757 step 6): an
/// effect's from its values, a grade node's from its settings, by the
/// catalogue's field for it. `None` for one that isn't a number.
pub fn node_number(n: &klipp_graph_format::Node, key: &str) -> Option<f64> {
    use klipp_graph_format::types;
    let (desc, values) = match n.type_id.as_str() {
        types::LEGACY_EFFECT => {
            let t = klipp_effects::registry::effect_type(klipp_graph_lift::effect_params::tag(n)?)?;
            (&t.desc, klipp_graph_lift::effect_params::entry_of(n).ok()?)
        }
        types::LEGACY_NODE => {
            let run = crate::grade_node_for_run(n).ok()?;
            (&klipp_nodes::registry::node_type(run.kind)?.desc, serde_json::to_value(&run.params).ok()?)
        }
        _ => return None,
    };
    let p = desc.params.iter().find(|p| p.key == key)?;
    let mut v = values.pointer(&format!("/{}", p.field.replace('.', "/")))?;
    if let Some(i) = p.index {
        v = v.get(i)?;
    }
    v.as_f64()
}

/// A template value as a graph op's parameter takes it.
fn param_value(value: &TemplateValue) -> serde_json::Value {
    match value {
        TemplateValue::Text(t) | TemplateValue::Enum(t) => serde_json::Value::from(t.clone()),
        TemplateValue::Color(c) => serde_json::json!(c),
        TemplateValue::Number(n) => serde_json::json!(n),
        TemplateValue::Bool(b) => serde_json::Value::from(*b),
    }
}

impl CompositionTemplate {
    /// The instance a recipe placed with `values` keeps (#2758): its
    /// controls where `values` set them (else their defaults), and each
    /// control's links measured where it is `placed`; a number control
    /// bound straight is a link across its own range.
    pub fn recipe_instance(&self, values: &BTreeMap<String, TemplateValue>, placed: &Placing) -> klipp_core::TemplateInstance {
        let mut instance = instance_for(self);
        let linked = self.bindings.iter().filter(|b| b.link.is_some() && self.parameters.contains_key(&b.parameter));
        for (l, b) in instance.links.iter_mut().zip(linked) {
            l.to = placed.measured(b.link.as_ref().and_then(|m| m.per), l.to);
        }
        // A number control set straight onto its node stays live too: a
        // link across its own range.
        for b in self.bindings.iter().filter(|b| b.link.is_none()) {
            let Some(p) = self.parameters.get(&b.parameter).filter(|p| matches!(p.default, TemplateValue::Number(_))) else { continue };
            let range = [p.range.as_ref().and_then(|r| r.min).unwrap_or(0.0), p.range.as_ref().and_then(|r| r.max).unwrap_or(1.0)];
            instance.bindings.remove(&b.parameter);
            instance.links.push(klipp_core::TemplateLink { parameter: b.parameter.clone(), property: b.property.clone(), from: range, to: range, curve: klipp_core::LinkCurve::Linear, offset: 0.0, linked: true, frozen: 0.0 });
            instance.overrides.entry(b.parameter.clone()).or_insert_with(|| p.default.clone());
        }
        for (name, v) in values.iter().filter(|(n, _)| self.parameters.contains_key(*n)) {
            instance.overrides.insert(name.clone(), v.clone());
        }
        instance
    }

    /// The graph ops that build this recipe into the picture of clip
    /// `clip` (#2758): its anchors resolved against `anchors`
    /// ([`recipe_anchors`]), each new node's path rooted at the clip, and
    /// each binding's value set on its node: the value given in `values`
    /// by parameter or slot name, else the parameter's default; a
    /// control's link maps its number onto the node's range. A slot left
    /// unfilled is refused, by name. A link is measured where the recipe
    /// is `placed` ([`Per`]); a time past the clip's end is refused, said.
    pub fn recipe_ops(&self, clip: Uuid, anchors: &BTreeMap<&str, Uuid>, values: &BTreeMap<String, TemplateValue>, placed: &Placing) -> Result<Vec<serde_json::Value>, String> {
        let graph = self.graph.as_ref().filter(|_| self.kind == TemplateKind::Recipe).ok_or_else(|| format!("\"{}\" is not a recipe", self.name))?;
        for name in values.keys() {
            if !self.parameters.contains_key(name) && !self.slots.contains_key(name) {
                return Err(format!("\"{}\" has no control or slot \"{name}\"", self.name));
            }
        }
        let mut ops = graph.ops.clone();
        let root = serde_json::json!({ "clip": clip });
        for op in &mut ops {
            let Some(fields) = op.as_object_mut() else { return Err("a recipe op is an object".into()) };
            for field in PATH_FIELDS {
                let Some(at) = fields.get_mut(field) else { continue };
                // move_after's `after` is a node id, not a path.
                if field == "after" && at.get("anchor").is_none() && at.get("new").is_none() {
                    continue;
                }
                let node = if let Some(name) = at.get("anchor").and_then(|a| a.as_str()) {
                    let id = anchors.get(name).ok_or_else(|| format!("the clip's picture has no {name} to attach to"))?;
                    serde_json::json!(id)
                } else if let Some(k) = at.get("new") {
                    serde_json::json!({ "new": k })
                } else {
                    return Err(format!("a recipe's {field} is an anchor or a new node"));
                };
                *at = serde_json::json!({ "root": root, "nodes": [node] });
            }
        }
        for b in &self.bindings {
            let direct = keys_op_binding(&b.property);
            let (k, param) = match direct {
                Some((op, _, _)) => (op, ""),
                None => node_binding(&b.property).ok_or_else(|| format!("a recipe binds node:<op>.<param> or keys:<op>#<key>.<field>, not '{}'", b.property))?,
            };
            let value = match (values.get(&b.parameter), self.parameters.get(&b.parameter), self.slots.get(&b.parameter)) {
                (Some(v), Some(p), _) => {
                    p.validate(v).map_err(|e| format!("control '{}': {e}", b.parameter))?;
                    v.clone()
                }
                (Some(v), None, Some(_)) => v.clone(),
                (None, Some(p), _) => p.default.clone(),
                (None, None, Some(slot)) => return Err(format!("fill the slot '{}' ({})", b.parameter, slot.label)),
                _ => return Err(format!("binding references unknown parameter '{}'", b.parameter)),
            };
            let value = match (&b.link, &value) {
                (Some(link), TemplateValue::Number(n)) => {
                    let range = self.parameters.get(&b.parameter).and_then(|p| p.range.as_ref());
                    let from = [range.and_then(|r| r.min).unwrap_or(0.0), range.and_then(|r| r.max).unwrap_or(1.0)];
                    if link.per == Some(Per::Source) && !(0.0..placed.length).contains(n) {
                        return Err(format!("control '{}': {n} s is past the clip's end ({:.2} s)", b.parameter, placed.length));
                    }
                    let to = placed.measured(link.per, link.to);
                    let mapped = klipp_core::TemplateLink { parameter: b.parameter.clone(), property: b.property.clone(), from, to, curve: link.curve, offset: 0.0, linked: true, frozen: 0.0 };
                    TemplateValue::Number(mapped.effective(*n))
                }
                (Some(_), _) => return Err(format!("control '{}' drives a link, so it is a number", b.parameter)),
                (None, v) => v.clone(),
            };
            let value = taken(&value, b.take).map_err(|e| format!("'{}': {e}", b.property))?;
            // A key of a keys op named by its own place.
            if let Some((op, at, field)) = direct {
                let set = match (field, &value) {
                    ("ease", TemplateValue::Text(t) | TemplateValue::Enum(t)) => serde_json::json!(t),
                    ("ease", _) => return Err(format!("control '{}' sets a key's ease, so it is a name", b.parameter)),
                    (_, TemplateValue::Number(n)) => serde_json::json!(n),
                    _ => return Err(format!("control '{}' sets a key's {field}, so it is a number", b.parameter)),
                };
                let key = ops.get_mut(op).filter(|o| o["op"] == "keys").and_then(|o| o["keys"].get_mut(at)).and_then(|m| m.as_object_mut()).ok_or_else(|| format!("'{}': op {op} is no keys op with a key {at}", b.property))?;
                key.insert(field.to_owned(), set);
                continue;
            }
            // A key's time or value, on the keys op that puts it there.
            if let Some((_, keyed, at, field)) = key_binding(&b.property) {
                let TemplateValue::Number(n) = value else { return Err(format!("control '{}' sets a key's {field}, so it is a number", b.parameter)) };
                let op = ops.iter_mut().find(|o| keys_on(o, k, keyed)).ok_or_else(|| format!("'{}' names no key a keys op puts on op {k}'s {keyed}", b.property))?;
                let key = op["keys"].get_mut(at).and_then(|m| m.as_object_mut()).ok_or_else(|| format!("'{}': its keys op has no key {at}", b.property))?;
                key.insert(field.to_owned(), serde_json::json!(n));
                continue;
            }
            // A behaviour's field, on the modify op that puts it there.
            if let Some((_, behaviour_of, field)) = behaviour_binding(&b.property) {
                let op = ops.iter_mut().find(|o| modifies(o, k, behaviour_of)).ok_or_else(|| format!("'{}' names no behaviour a modify op puts on op {k}'s {behaviour_of}", b.property))?;
                let first = op["modifiers"].get_mut(0).and_then(|m| m.as_object_mut()).ok_or_else(|| format!("'{}': its modify op puts no behaviour there", b.property))?;
                first.insert(field.to_owned(), param_value(&value));
                continue;
            }
            let op = ops.get_mut(k).and_then(|o| o.as_object_mut()).ok_or_else(|| format!("'{}' names op {k}, which the recipe lacks", b.property))?;
            let params = op.entry("params").or_insert_with(|| serde_json::json!({}));
            // A value inside a parameter, where the recipe writes it.
            if let Some((head, pointer)) = nested_binding(param) {
                let at = params.get_mut(head).and_then(|v| v.pointer_mut(&pointer)).ok_or_else(|| format!("'{}': op {k}'s {head} has nothing at {pointer}", b.property))?;
                *at = param_value(&value);
                continue;
            }
            params[param] = param_value(&value);
        }
        Ok(ops)
    }
}

/// One slot of a template or a recipe (#2757, #2758): what the user calls
/// it and what fills it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateSlot {
    pub label: String,
    pub kind: SlotKind,
}

/// What fills a slot. A template fills media slots; the Composite page's
/// recipes add the others (#2758).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    /// A media item from the pool: a picture or a video.
    Media,
    /// A clip on the timeline.
    Clip,
    /// A title.
    Title,
    /// A subject matte of a clip.
    SubjectMatte,
    /// A colour.
    Colour,
    /// A surface tracked in the clip the recipe is placed on (a sign, a
    /// screen): its name or id.
    Surface,
}

/// The new node a recipe op's path names, `{"new": k}` or a path whose
/// one node is that.
fn new_node_of(path: &serde_json::Value) -> Option<usize> {
    let at = path.get("new").or_else(|| path["nodes"].as_array().filter(|n| n.len() == 1).and_then(|n| n[0].get("new")))?;
    at.as_u64().and_then(|k| usize::try_from(k).ok())
}

impl CompositionTemplate {
    /// The ids its recipe graph names, read as they are now (#2888): a
    /// renamed type, parameter or port as its new id, through
    /// [`crate::registry_ids::MIGRATIONS`]. A type this Klipp doesn't
    /// have, one removed, or renames that loop, refuse the file, saying
    /// which. A parameter or port key that isn't declared is left as it
    /// is: the graph op that places the recipe refuses it by name.
    pub fn migrate_ids(&mut self) -> Result<(), String> {
        self.migrate_ids_in(crate::registry_ids::MIGRATIONS)
    }

    /// [`Self::migrate_ids`] through `table`.
    fn migrate_ids_in(&mut self, table: &[crate::registry_ids::Migration]) -> Result<(), String> {
        use crate::registry_ids::{is_current, resolve_in, Resolved, Subject};
        let resolve = |s: Subject<'_>| resolve_in(table, s, &is_current);
        let Some(graph) = self.graph.as_mut() else { return Ok(()) };
        let mut types: Vec<Option<String>> = Vec::with_capacity(graph.ops.len());
        for (k, op) in graph.ops.iter_mut().enumerate() {
            let Some(t) = op.get("type").and_then(|t| t.as_str()).map(str::to_owned) else {
                types.push(None);
                continue;
            };
            let now = match resolve(Subject::Type(&t)) {
                Resolved::Current(now) => now,
                Resolved::Removed(why) => return Err(format!("op {k}: {t} was removed: {why}")),
                Resolved::Unknown => return Err(format!("op {k}: {t} is not an effect or node this Klipp has")),
                Resolved::Loops => return Err(format!("op {k}: {t}'s renames run in a loop")),
            };
            op["type"] = now.clone().into();
            types.push(Some(now));
        }
        let key_now = |at: usize, key: &str, port: bool| -> Result<Option<String>, String> {
            let Some(ty) = types.get(at).and_then(|t| t.as_deref()) else { return Ok(None) };
            match resolve(if port { Subject::Port(ty, key) } else { Subject::Param(ty, key) }) {
                Resolved::Current(now) => Ok((now != key).then_some(now)),
                Resolved::Removed(why) => Err(format!("{ty}'s {key} was removed: {why}")),
                Resolved::Loops => Err(format!("{ty}'s {key}: its renames run in a loop")),
                Resolved::Unknown => Ok(None),
            }
        };
        for (k, op) in graph.ops.iter_mut().enumerate() {
            if let Some(params) = op.get_mut("params").and_then(|p| p.as_object_mut()) {
                for key in params.keys().cloned().collect::<Vec<_>>() {
                    if let Some(now) = key_now(k, &key, false)? {
                        let value = params.remove(&key).expect("a key just listed");
                        params.insert(now, value);
                    }
                }
            }
            if op["op"] == "modify" {
                if let (Some(at), Some(key)) = (new_node_of(&op["path"]), op["key"].as_str().map(str::to_owned)) {
                    if let Some(now) = key_now(at, &key, false)? {
                        op["key"] = now.into();
                    }
                }
            }
            if op["op"] == "connect" {
                for (end, port) in [("from", "from_port"), ("to", "to_port")] {
                    if let (Some(at), Some(key)) = (new_node_of(&op[end]), op[port].as_str().map(str::to_owned)) {
                        if let Some(now) = key_now(at, &key, true)? {
                            op[port] = now.into();
                        }
                    }
                }
            }
        }
        for b in &mut self.bindings {
            let Some((k, rest)) = node_binding(&b.property) else { continue };
            // The parameter, then what the binding reaches inside it: a
            // field (`~`), a value by JSON pointer (`/`) or a key (`#`).
            let (param, tail) = rest.split_at(rest.find(['~', '/', '#']).unwrap_or(rest.len()));
            if let Some(now) = key_now(k, param, false)? {
                b.property = format!("node:{k}.{now}{tail}");
            }
        }
        Ok(())
    }

    /// `json` read as a template file: its ids as they are now
    /// ([`Self::migrate_ids`]), then checked ([`Self::validate`]).
    pub fn read(json: &str) -> Result<Self, String> {
        let mut t: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        t.within_limits()?;
        t.migrate_ids()?;
        t.validate()?;
        Ok(t)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.within_limits()?;
        if self.format_version > FORMAT_VERSION {
            return Err(format!("written by a newer Klipp (format {}, this one reads {FORMAT_VERSION})", self.format_version));
        }
        if self.format_version < FORMAT_VERSION {
            return Err(format!("format {} only named its composition: open the project it was made in and save it again", self.format_version));
        }
        match self.kind {
            TemplateKind::Composition if self.composition().is_none() => {
                return Err("the template does not carry its composition".into());
            }
            TemplateKind::Recipe => {
                let graph = self.graph.as_ref().filter(|g| !g.ops.is_empty()).ok_or("a recipe carries the graph it builds")?;
                // The placement is keyed, never built onto.
                for op in &graph.ops {
                    let placed_by = PATH_FIELDS.iter().filter(|f| op.get(**f).and_then(|p| p.get("anchor")).and_then(|a| a.as_str()) == Some("placement")).copied().collect::<Vec<_>>();
                    if !placed_by.is_empty() && (op["op"] != "keys" || placed_by != ["path"]) {
                        return Err("only a keys op names the placement".into());
                    }
                }
                for b in &self.bindings {
                    if let Some((op, at, field)) = keys_op_binding(&b.property) {
                        let keys = graph.ops.get(op).filter(|o| o["op"] == "keys").ok_or_else(|| format!("'{}' names op {op}, which is no keys op", b.property))?;
                        if keys["keys"].get(at).is_none() {
                            return Err(format!("'{}': op {op} has no key {at}", b.property));
                        }
                        let named = self.parameters.get(&b.parameter).is_some_and(|p| matches!(p.default, TemplateValue::Text(_) | TemplateValue::Enum(_)));
                        if (field == "ease") != named {
                            return Err(format!("'{}' sets a key's {field}, so '{}' is {}", b.property, b.parameter, if field == "ease" { "a name" } else { "a number" }));
                        }
                        continue;
                    }
                    let (k, _) = node_binding(&b.property).ok_or_else(|| format!("a recipe binds node:<op>.<param> or keys:<op>#<key>.<field>, not '{}'", b.property))?;
                    let adds = graph.ops.get(k).and_then(|o| o.get("op")).and_then(|o| o.as_str()).is_some_and(|o| matches!(o, "add" | "add_beside"));
                    if !adds {
                        return Err(format!("'{}' names op {k}, which adds no node", b.property));
                    }
                    let colour = self.parameters.get(&b.parameter).is_some_and(|p| matches!(p.default, TemplateValue::Color(_)))
                        || self.slots.get(&b.parameter).is_some_and(|s| s.kind == SlotKind::Colour);
                    if b.take == Some(Take::Hue) && !colour {
                        return Err(format!("'{}' takes a hue, so '{}' is a colour", b.property, b.parameter));
                    }
                    if let Some((_, param, _)) = behaviour_binding(&b.property) {
                        if !graph.ops.iter().any(|o| modifies(o, k, param)) {
                            return Err(format!("'{}' names a behaviour no modify op puts on op {k}'s {param}", b.property));
                        }
                    }
                    if let Some((_, keyed, at, _)) = key_binding(&b.property) {
                        if !graph.ops.iter().any(|o| keys_on(o, k, keyed) && o["keys"].get(at).is_some()) {
                            return Err(format!("'{}' names no key {at} a keys op puts on op {k}'s {keyed}", b.property));
                        }
                        if self.parameters.get(&b.parameter).is_some_and(|p| !matches!(p.default, TemplateValue::Number(_))) {
                            return Err(format!("'{}' sets a key, so '{}' is a number", b.property, b.parameter));
                        }
                    }
                    let (_, param) = node_binding(&b.property).expect("checked above");
                    if let Some((head, pointer)) = nested_binding(param) {
                        if behaviour_binding(&b.property).is_some() {
                            return Err(format!("'{}' binds a behaviour or a value inside a parameter, not both", b.property));
                        }
                        if graph.ops[k].get("params").and_then(|p| p.get(head)).and_then(|v| v.pointer(&pointer)).is_none() {
                            return Err(format!("'{}': op {k}'s {head} has nothing at {pointer}", b.property));
                        }
                    }
                }
            }
            TemplateKind::AudioChain => {
                let chain = self.audio.as_ref().filter(|a| !a.steps.is_empty()).ok_or("an audio chain recipe carries the steps it adds")?;
                if chain.steps.len() > MAX_AUDIO_STEPS {
                    return Err(format!("an audio chain adds at most {MAX_AUDIO_STEPS} steps, not {}", chain.steps.len()));
                }
                let mut names = std::collections::BTreeSet::new();
                for step in &chain.steps {
                    if step.name.trim().is_empty() || !names.insert(step.name.to_lowercase()) {
                        return Err(format!("a step's name \"{}\" is blank or twice", step.name));
                    }
                    // Klipp's own effects only: never a plugin (an LV2 URI,
                    // an OFX bundle) a file names.
                    let spec = klipp_core::audio_effect_registry::audio_effect(&step.effect).ok_or_else(|| format!("step \"{}\": \"{}\" is not one of Klipp's audio effects", step.name, step.effect))?;
                    if let Some(k) = step.params.keys().find(|k| !spec.params.contains(&k.as_str())) {
                        return Err(format!("step \"{}\": {} has no setting \"{k}\"", step.name, spec.name));
                    }
                    // Each in range is the effect's own to keep: it clamps
                    // every setting it is given.
                    if step.params.values().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&step.wet_dry) {
                        return Err(format!("step \"{}\": each setting is a finite number and the mix 0 to 1", step.name));
                    }
                }
                for b in &self.bindings {
                    let (step, param) = step_binding(&b.property).ok_or_else(|| format!("an audio chain binds step:<name>.<param>, not '{}'", b.property))?;
                    let found = chain.steps.iter().find(|s| s.name.eq_ignore_ascii_case(step)).ok_or_else(|| format!("'{}' names no step \"{step}\"", b.property))?;
                    if !klipp_core::audio_effect_registry::audio_effect(&found.effect).is_some_and(|s| s.params.contains(&param)) {
                        return Err(format!("'{}': {} has no setting \"{param}\"", b.property, found.effect));
                    }
                    if b.link.is_some() || self.parameters.get(&b.parameter).is_some_and(|p| !matches!(p.default, TemplateValue::Number(_))) {
                        return Err(format!("'{}' sets a setting, so '{}' is a number, bound as it is", b.property, b.parameter));
                    }
                }
            }
            TemplateKind::Clips => {
                let recipe = self.clips.as_ref().filter(|c| !c.steps.is_empty()).ok_or("a clips recipe carries the steps it makes")?;
                if recipe.steps.len() > MAX_CLIP_STEPS {
                    return Err(format!("a clips recipe makes at most {MAX_CLIP_STEPS} steps, not {}", recipe.steps.len()));
                }
                if let Some((name, _)) = self.slots.iter().find(|(_, s)| s.kind != SlotKind::Clip) {
                    return Err(format!("slot \"{name}\": a clips recipe's slots are clips"));
                }
                if let Some(name) = recipe.find.keys().find(|n| !self.slots.contains_key(*n)) {
                    return Err(format!("it finds \"{name}\", which is not one of its slots"));
                }
                let names_clip = |c: &str| c == "placed" || self.slots.contains_key(c);
                for (k, step) in recipe.steps.iter().enumerate() {
                    if !names_clip(&step.clip) {
                        return Err(format!("step {k} is on \"{}\", neither placed nor one of its slots", step.clip));
                    }
                    match (&step.matte, &step.keys) {
                        (Some(m), None) => {
                            if m.source == step.clip || !self.slots.contains_key(&m.source) {
                                return Err(format!("step {k}'s matte is seen through \"{}\", which is not another of its slots", m.source));
                            }
                            if m.channel.as_deref().is_some_and(|c| !matches!(c, "auto" | "alpha" | "luma")) {
                                return Err(format!("step {k}'s matte channel is auto, alpha or luma"));
                            }
                            if !(m.feather_px.is_finite() && m.choke_px.is_finite() && m.feather_px >= 0.0 && m.choke_px >= 0.0) {
                                return Err(format!("step {k}'s feather and choke are finite pixels, 0 or more"));
                            }
                        }
                        (None, Some(keys)) => {
                            if clip_key_param(&keys.param).is_none() {
                                return Err(format!("step {k} keys \"{}\": it keys {}", keys.param, CLIP_KEY_PARAMS.map(|(n, _)| n).join(", ")));
                            }
                            if keys.keys.is_empty() || keys.keys.len() > MAX_CLIP_KEYS {
                                return Err(format!("step {k} has {} keys, from 1 to {MAX_CLIP_KEYS}", keys.keys.len()));
                            }
                            if keys.keys.iter().any(|key| !key.at.finite() || !key.value.finite()) {
                                return Err(format!("step {k}'s keys are at finite times, 0 or more, with finite values"));
                            }
                        }
                        _ => return Err(format!("step {k} is a matte or keys, one of them")),
                    }
                }
                for b in &self.bindings {
                    let (k, j, _) = clip_key_binding(&b.property).ok_or_else(|| format!("a clips recipe binds steps.<i>.keys.<j>.value|at, not '{}'", b.property))?;
                    if recipe.steps.get(k).and_then(|s| s.keys.as_ref()).is_none_or(|keys| keys.keys.len() <= j) {
                        return Err(format!("'{}' names no key {j} of a keys step {k}", b.property));
                    }
                    if b.link.is_some() || self.parameters.get(&b.parameter).is_some_and(|p| !matches!(p.default, TemplateValue::Number(_))) {
                        return Err(format!("'{}' sets a key, so '{}' is a number, bound as it is", b.property, b.parameter));
                    }
                }
            }
            TemplateKind::Composition => {
                // Each format's layout carried, and every binding and
                // control resolving in it as in the main one.
                for (key, id) in &self.formats {
                    if !matches!(key.as_str(), "tall" | "square") {
                        return Err(format!("format '{key}' is not tall or square"));
                    }
                    let layout = self.sequences.iter().find(|s| s.id.0 == *id).ok_or_else(|| format!("the {key} layout is not carried"))?;
                    let mut instance = instance_for(self);
                    instance.bindings.retain(|_, p| !p.starts_with("clip:"));
                    crate::flatten::validate_template_instance(&instance, layout).map_err(|e| format!("the {key} layout: {e}"))?;
                }
            }
        }
        if self.name.trim().is_empty() {
            return Err("a template needs a name".into());
        }
        for (name, parameter) in &self.parameters {
            parameter
                .validate(&parameter.default)
                .map_err(|error| format!("parameter '{name}': {error}"))?;
        }
        for binding in &self.bindings {
            if !self.parameters.contains_key(&binding.parameter) && !self.slots.contains_key(&binding.parameter) {
                return Err(format!(
                    "binding references unknown parameter '{}'",
                    binding.parameter
                ));
            }
            if binding.property.trim().is_empty() {
                return Err(format!(
                    "binding for '{}' has no property",
                    binding.parameter
                ));
            }
        }
        Ok(())
    }

    /// A template file is read from a folder anyone can drop one into
    /// (#2758): one asking for more than any made here holds is refused
    /// before anything is built from it.
    fn within_limits(&self) -> Result<(), String> {
        let count = |what: &str, n: usize, most: usize| if n > most { Err(format!("a template holds at most {most} {what}, not {n}")) } else { Ok(()) };
        count("parameters", self.parameters.len(), MAX_PARAMETERS)?;
        count("bindings", self.bindings.len(), MAX_BINDINGS)?;
        count("slots", self.slots.len(), MAX_SLOTS)?;
        count("sequences", self.sequences.len(), MAX_SEQUENCES)?;
        if let Some(c) = &self.clips {
            count("clip steps", c.steps.len(), MAX_CLIP_STEPS)?;
            for step in &c.steps {
                count("keys in one step", step.keys.as_ref().map_or(0, |k| k.keys.len()), MAX_CLIP_KEYS)?;
            }
        }
        if let Some(a) = &self.audio {
            count("audio steps", a.steps.len(), MAX_AUDIO_STEPS)?;
            for step in &a.steps {
                count("settings in one step", step.params.len(), MAX_PARAMETERS)?;
            }
        }
        if let Some(g) = &self.graph {
            count("graph ops", g.ops.len(), MAX_RECIPE_OPS)?;
            count("checks", g.checks.len(), MAX_RECIPE_CHECKS)?;
            // Keys from someone else's file: a bounded count, each with a
            // finite time and value.
            for op in g.ops.iter().filter(|o| o["op"] == "keys") {
                let keys = op["keys"].as_array().ok_or("a keys op carries a list of keys")?;
                count("keys on one parameter", keys.len(), MAX_RECIPE_KEYS)?;
                if keys.iter().any(|k| !["time_secs", "value"].iter().all(|f| k.get(f).and_then(serde_json::Value::as_f64).is_some_and(f64::is_finite))) {
                    return Err("each key of a keys op has a finite time_secs and value".into());
                }
            }
            count("check values", g.check_with.len(), MAX_PARAMETERS)?;
        }
        let texts = [self.name.as_str(), self.category.as_str(), self.written_by.as_str()]
            .into_iter()
            .chain(self.parameters.iter().flat_map(|(k, p)| [k.as_str(), p.label.as_str()]))
            .chain(self.parameters.values().flat_map(|p| p.choices.iter().map(String::as_str)))
            .chain(self.slots.iter().flat_map(|(k, s)| [k.as_str(), s.label.as_str()]))
            .chain(self.bindings.iter().flat_map(|b| [b.parameter.as_str(), b.property.as_str()]))
            .chain(self.graph.iter().flat_map(|g| g.checks.iter().map(|c| c.says.as_str()).chain(g.check_with.keys().map(String::as_str))))
            .chain(self.audio.iter().flat_map(|a| a.steps.iter().flat_map(|s| [s.name.as_str(), s.effect.as_str(), s.says.as_str()].into_iter().chain(s.params.keys().map(String::as_str)))))
            .chain(self.clips.iter().flat_map(|c| c.find.keys().map(String::as_str).chain(c.steps.iter().flat_map(|s| std::iter::once(s.clip.as_str()).chain(s.matte.iter().flat_map(|m| std::iter::once(m.source.as_str()).chain(m.channel.as_deref()))).chain(s.keys.iter().map(|k| k.param.as_str()))))));
        for t in texts {
            if t.len() > MAX_TEXT || t.chars().any(char::is_control) {
                return Err(format!("a name or label of {} bytes, or with control characters, is refused", t.len()));
            }
        }
        Ok(())
    }

    pub fn new(name: impl Into<String>, composition_id: Uuid) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            id: Uuid::new_v4(),
            name: name.into(),
            category: String::new(),
            composition_id,
            parameters: BTreeMap::new(),
            bindings: Vec::new(),
            written_by: env!("CARGO_PKG_VERSION").to_string(),
            sequences: Vec::new(),
            slots: BTreeMap::new(),
            kind: TemplateKind::Composition,
            graph: None,
            audio: None,
            clips: None,
            formats: BTreeMap::new(),
        }
    }
}


impl CompositionTemplate {
    /// An audio chain recipe's steps as the effects it adds (#2886):
    /// those named in `only` (any case), else all, in the recipe's order,
    /// each with its name, what it says, and its effect with its settings
    /// in the order the effect lists them, a bound one from `values` (a
    /// control's number, its default when not given). A step `only`
    /// names that the recipe hasn't got is refused with those it has.
    pub fn audio_steps(&self, values: &BTreeMap<String, TemplateValue>, only: Option<&[String]>) -> Result<Vec<(String, String, klipp_core::AudioEffectInstance)>, String> {
        let chain = self.audio.as_ref().filter(|_| self.kind == TemplateKind::AudioChain).ok_or_else(|| format!("\"{}\" is not an audio chain", self.name))?;
        if let Some(want) = only {
            if let Some(w) = want.iter().find(|w| !chain.steps.iter().any(|s| s.name.eq_ignore_ascii_case(w.trim()))) {
                return Err(format!("\"{}\" has no step \"{w}\"; it has {}", self.name, chain.steps.iter().map(|s| format!("\"{}\"", s.name)).collect::<Vec<_>>().join(", ")));
            }
        }
        let mut out = Vec::new();
        for step in chain.steps.iter().filter(|s| only.is_none_or(|w| w.iter().any(|w| s.name.eq_ignore_ascii_case(w.trim())))) {
            let spec = klipp_core::audio_effect_registry::audio_effect(&step.effect).ok_or_else(|| format!("\"{}\" is not one of Klipp's audio effects", step.effect))?;
            let mut params = step.params.clone();
            for b in &self.bindings {
                let Some((on, param)) = step_binding(&b.property) else { continue };
                if !on.eq_ignore_ascii_case(&step.name) {
                    continue;
                }
                let value = values.get(&b.parameter).or_else(|| self.parameters.get(&b.parameter).map(|p| &p.default));
                if let Some(TemplateValue::Number(v)) = value {
                    params.insert(param.to_string(), *v as f32);
                }
            }
            let ordered = spec.params.iter().filter_map(|k| params.get(*k).map(|v| (k.to_string(), *v))).collect();
            out.push((step.name.clone(), step.says.clone(), klipp_core::AudioEffectInstance { effect_type: step.effect.clone(), enabled: true, wet_dry: step.wet_dry, params: ordered }));
        }
        Ok(out)
    }

    /// The composition's sequence the template carries.
    pub fn composition(&self) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.id.0 == self.composition_id)
    }

    /// The composition for a `width` × `height` frame (#2757): its own
    /// layout for a tall or square frame when it has one, else the main.
    pub fn composition_for(&self, width: u32, height: u32) -> Option<&Sequence> {
        let key = match crate::title_library::Format::of(width, height) {
            crate::title_library::Format::Tall => "tall",
            crate::title_library::Format::Square => "square",
            crate::title_library::Format::Wide => return self.composition(),
        };
        self.formats.get(key).and_then(|id| self.sequences.iter().find(|s| s.id.0 == *id)).or_else(|| self.composition())
    }
}

/// What a scene can expose as a template's parameter or slot (#2757): its
/// name in the template, what the user calls it, the parameter (`None`
/// for a media slot) and the property it is bound to.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub label: String,
    pub parameter: Option<TemplateParameter>,
    pub property: String,
}

impl Candidate {
    /// Whether it is offered ticked: a text, and a media slot.
    pub fn by_default(&self) -> bool {
        self.parameter.is_none() || matches!(self.parameter.as_ref().map(|p| &p.default), Some(TemplateValue::Text(_)) if self.property.ends_with(".text"))
    }
}

fn text_param(label: String, default: TemplateValue) -> Option<TemplateParameter> {
    Some(TemplateParameter { label, default, range: None, choices: Vec::new() })
}

/// What `scene` can expose (#2757), in its tracks' order: each title
/// layer's text, colour and font, each image layer's picture, and each
/// media clip as a slot.
pub fn candidates(scene: &Sequence) -> Vec<Candidate> {
    use crate::{ClipKind, TitleObjectContent};
    let mut out = Vec::new();
    let mut object = 0usize;
    let mut texts = 0usize;
    let mut slots = 0usize;
    for (n, clip) in scene.tracks.iter().flat_map(|t| &t.clips).enumerate() {
        match &clip.kind {
            ClipKind::Title(title) => {
                for o in &title.objects {
                    let i = object;
                    object += 1;
                    let named = |fallback: String| if o.name.trim().is_empty() { fallback } else { o.name.trim().to_string() };
                    let colour = |c: [u8; 4]| TemplateValue::Color(c.map(|v| v as f32 / 255.0));
                    match &o.content {
                        TitleObjectContent::TextBox(tb) => {
                            texts += 1;
                            let label = named(format!("Text {texts}"));
                            out.push(Candidate { name: format!("text_{i}"), label: label.clone(), parameter: text_param(label.clone(), TemplateValue::Text(tb.text.clone())), property: format!("object:{i}.text") });
                            out.push(Candidate { name: format!("color_{i}"), label: format!("{label} colour"), parameter: text_param(format!("{label} colour"), colour(tb.color)), property: format!("object:{i}.color") });
                            out.push(Candidate { name: format!("font_{i}"), label: format!("{label} font"), parameter: text_param(format!("{label} font"), TemplateValue::Text(tb.font_name.clone())), property: format!("object:{i}.font") });
                        }
                        TitleObjectContent::Shape(shape) => {
                            if let Some(fill) = shape.fill {
                                let label = named(format!("Shape {}", i + 1));
                                out.push(Candidate { name: format!("color_{i}"), label: format!("{label} colour"), parameter: text_param(format!("{label} colour"), colour(fill)), property: format!("object:{i}.color") });
                            }
                        }
                        TitleObjectContent::Image(img) => {
                            let label = named(img.stem());
                            out.push(Candidate { name: format!("picture_{i}"), label: label.clone(), parameter: text_param(label, TemplateValue::Text(img.path.clone())), property: format!("object:{i}.picture") });
                        }
                    }
                }
            }
            ClipKind::Media { .. } => {
                slots += 1;
                out.push(Candidate { name: format!("slot_{slots}"), label: format!("Media {slots}"), parameter: None, property: format!("clip:{n}.media") });
            }
            _ => {}
        }
    }
    out
}

/// The numbers a control can drive in `scene` (#2757), by label and
/// property: each title layer's size, tracking and line spacing (text),
/// opacity, position, rotation, motion lengths and its first glow's
/// radius and strength (#3304). Named after the
/// layer, the way `candidates` names it ("Text 1 size").
pub fn number_targets(scene: &Sequence) -> Vec<(String, String)> {
    use crate::TitleObjectContent;
    let mut out = Vec::new();
    let (mut texts, mut i) = (0usize, 0usize);
    for o in scene.tracks.iter().flat_map(|t| &t.clips).filter_map(|c| c.title_data()).flat_map(|t| t.objects.iter()) {
        let fallback = match &o.content {
            TitleObjectContent::TextBox(_) => { texts += 1; format!("Text {texts}") }
            TitleObjectContent::Shape(_) => format!("Shape {}", i + 1),
            TitleObjectContent::Image(img) => img.stem(),
        };
        let label = if o.name.trim().is_empty() { fallback } else { o.name.trim().to_string() };
        let mut paths = Vec::new();
        if matches!(o.content, TitleObjectContent::TextBox(_)) {
            paths.extend([("size", "size"), ("tracking", "tracking"), ("line spacing", "line_spacing")]);
        }
        paths.extend([("opacity", "opacity"), ("x", "x"), ("y", "y"), ("rotation", "rotation")]);
        if o.fx().motion.enter.is_some() { paths.push(("entrance length", "motion.enter.duration_secs")); }
        if o.fx().motion.exit.is_some() { paths.push(("exit length", "motion.exit.duration_secs")); }
        if !o.fx().glows.is_empty() { paths.extend([("glow", "glow.radius"), ("glow strength", "glow.intensity")]); }
        out.extend(paths.into_iter().map(|(name, path)| (format!("{label} {name}"), format!("object:{i}.{path}"))));
        i += 1;
    }
    out
}

/// A macro control a template is saved with (#2757): a slider from `min`
/// to `max` (0 to 1 when left out) starting at `default`, driving each
/// target, named by its `number_targets` label, across `to` on `curve`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlSpec {
    pub label: String,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub default: Option<f64>,
    pub drives: Vec<DriveSpec>,
}

/// One target of a `ControlSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriveSpec {
    pub target: String,
    pub to: [f64; 2],
    #[serde(default)]
    pub curve: klipp_core::LinkCurve,
}

/// The name a control labelled `label` is saved by (#2757): its words in
/// lower case joined by `_`, so ops can name it either way.
pub fn control_name(label: &str) -> String {
    label.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>().join("_")
}

/// Adds `controls` to `t`, a template of `scene` (#2757). A target no
/// layer has, a range that is not a range, or a label taken is refused,
/// naming the control and the target.
pub fn add_controls(t: &mut CompositionTemplate, scene: &Sequence, controls: &[ControlSpec]) -> Result<(), String> {
    let targets = number_targets(scene);
    for c in controls {
        let label = c.label.trim();
        if label.is_empty() {
            return Err("a control needs a label".into());
        }
        if t.parameters.values().any(|p| p.label.eq_ignore_ascii_case(label)) {
            return Err(format!("control \"{label}\": the template already has a field of that name"));
        }
        let (min, max) = (c.min.unwrap_or(0.0), c.max.unwrap_or(1.0));
        let default = c.default.unwrap_or(min);
        if !(min.is_finite() && max.is_finite() && min < max) || !(min..=max).contains(&default) {
            return Err(format!("control \"{label}\": min must be below max, with the default between them"));
        }
        if c.drives.is_empty() {
            return Err(format!("control \"{label}\" drives nothing: give drives with a target each"));
        }
        let name = control_name(label);
        if t.parameters.contains_key(&name) {
            return Err(format!("control \"{label}\": the template already has a field or control called that"));
        }
        for d in &c.drives {
            let property = targets.iter().find(|(l, p)| l.eq_ignore_ascii_case(d.target.trim()) || p == d.target.trim()).map(|(_, p)| p.clone()).ok_or_else(|| {
                format!("control \"{label}\" drives \"{}\", which the clips do not have: they have {}", d.target, targets.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(", "))
            })?;
            if !d.to.iter().all(|v| v.is_finite()) {
                return Err(format!("control \"{label}\" drives \"{}\" to a range that is not two numbers", d.target));
            }
            t.bindings.push(TemplateBinding { parameter: name.clone(), property, link: Some(LinkMap { to: d.to, curve: d.curve, per: None }), take: None });
        }
        t.parameters.insert(name, TemplateParameter { label: label.to_string(), default: TemplateValue::Number(default), range: Some(NumberRange { min: Some(min), max: Some(max) }), choices: Vec::new() });
    }
    Ok(())
}

/// The clips `ids` of `seq` as a scene of their own (#2757): their tracks
/// kept in order, their times from the first one's start, the timeline
/// untouched. `None` when none is there.
pub fn scene_from_clips(seq: &Sequence, ids: &[klipp_core::ClipId]) -> Option<Sequence> {
    let picked: Vec<(usize, &crate::Clip)> = seq.tracks.iter().enumerate().flat_map(|(t, track)| track.clips.iter().filter(|c| ids.contains(&c.id)).map(move |c| (t, c))).collect();
    let start = picked.iter().map(|(_, c)| c.timeline_in).min()?;
    let mut scene = Sequence::new_default();
    scene.name = "Template".into();
    scene.fps = seq.fps;
    scene.resolution = seq.resolution;
    scene.tracks.clear();
    for (t, track) in seq.tracks.iter().enumerate() {
        let clips: Vec<crate::Clip> = picked.iter().filter(|(pt, _)| *pt == t).map(|(_, c)| {
            let mut c = (*c).clone();
            c.timeline_in -= start;
            c.timeline_out -= start;
            c
        }).collect();
        if !clips.is_empty() {
            let mut new = crate::Track::new(&track.name, track.kind);
            new.clips = clips;
            new.sort_clips();
            scene.tracks.push(new);
        }
    }
    Some(scene)
}

/// A template of `scene` called `name` (#2757), carrying it and what it
/// nests among `all`, exposing the candidates labelled in `chosen` (in
/// any case), or those offered ticked when none are named. An unknown
/// label is refused with the ones there are.
pub fn template_from_scene(scene: &Sequence, all: &[Sequence], name: &str, chosen: Option<&[String]>) -> Result<CompositionTemplate, String> {
    let offered = candidates(scene);
    let picked: Vec<&Candidate> = match chosen {
        None => offered.iter().filter(|c| c.by_default()).collect(),
        Some(labels) => labels.iter().map(|l| offered.iter().find(|c| c.label.eq_ignore_ascii_case(l.trim())).ok_or_else(|| {
            format!("the clips have no \"{l}\" to expose: they have {}", offered.iter().map(|c| c.label.as_str()).collect::<Vec<_>>().join(", "))
        })).collect::<Result<_, _>>()?,
    };
    let mut t = CompositionTemplate::new(name.trim(), scene.id.0);
    for c in picked {
        match &c.parameter {
            Some(p) => {
                t.parameters.insert(c.name.clone(), p.clone());
            }
            None => {
                t.slots.insert(c.name.clone(), TemplateSlot { label: c.label.clone(), kind: SlotKind::Media });
            }
        }
        t.bindings.push(TemplateBinding { parameter: c.name.clone(), property: c.property.clone(), link: None, take: None });
    }
    let mut with_scene: Vec<Sequence> = all.to_vec();
    with_scene.push(scene.clone());
    t.sequences = composition_sequences(scene, &with_scene);
    t.validate()?;
    Ok(t)
}

/// `root` and every sequence it nests, compound or precomposition, at any
/// depth, found among `all`: what a template made of `root` carries.
pub fn composition_sequences(root: &Sequence, all: &[Sequence]) -> Vec<Sequence> {
    let mut out: Vec<Sequence> = vec![root.clone()];
    let mut i = 0;
    while i < out.len() {
        let nested: Vec<klipp_core::SequenceId> = out[i]
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter_map(|c| match &c.kind {
                crate::ClipKind::Sequence { sequence_id } | crate::ClipKind::Precomposition { sequence_id } | crate::ClipKind::TemplatePrecomposition { sequence_id, .. } => Some(*sequence_id),
                _ => None,
            })
            .collect();
        for id in nested {
            if !out.iter().any(|s| s.id == id) {
                if let Some(s) = all.iter().find(|s| s.id == id) {
                    out.push(s.clone());
                }
            }
        }
        i += 1;
    }
    out
}

/// The composition templates Klipp ships (#2757), as files in
/// `assets/templates`: each read and checked as a saved one is.
pub fn builtin_templates() -> Vec<CompositionTemplate> {
    const FILES: &[&str] = &[
        include_str!("../../../assets/templates/word_reveal.klipptemplate"),
        include_str!("../../../assets/templates/line_wipe.klipptemplate"),
        include_str!("../../../assets/templates/number_card.klipptemplate"),
        include_str!("../../../assets/templates/countdown.klipptemplate"),
        include_str!("../../../assets/templates/top_five.klipptemplate"),
        include_str!("../../../assets/templates/hook_card.klipptemplate"),
        include_str!("../../../assets/templates/end_screen.klipptemplate"),
        include_str!("../../../assets/templates/photo_lower_third.klipptemplate"),
        include_str!("../../../assets/templates/banner_lower_third.klipptemplate"),
        include_str!("../../../assets/templates/split_screen.klipptemplate"),
        include_str!("../../../assets/templates/collage.klipptemplate"),
        include_str!("../../../assets/templates/stinger.klipptemplate"),
        include_str!("../../../assets/templates/opener.klipptemplate"),
        include_str!("../../../assets/templates/terminal_card.klipptemplate"),
        include_str!("../../../assets/templates/glow_caps.klipptemplate"),
    ];
    FILES
        .iter()
        .filter_map(|json| match CompositionTemplate::read(json) {
            Ok(t) => Some(t),
            Err(why) => {
                tracing::error!("a built-in template doesn't read: {why}");
                debug_assert!(false, "a built-in template doesn't read: {why}");
                None
            }
        })
        .collect()
}

/// The audio chain recipes Klipp ships (#2886), as files in
/// `assets/recipes`: each read and checked as a saved one is.
pub fn builtin_audio_chains() -> Vec<CompositionTemplate> {
    const FILES: [&str; 1] = [include_str!("../../../assets/recipes/clean_audio.klipptemplate")];
    FILES
        .iter()
        .filter_map(|json| match CompositionTemplate::read(json) {
            Ok(t) if t.kind == TemplateKind::AudioChain => Some(t),
            Ok(t) => {
                debug_assert!(false, "\"{}\" is not an audio chain", t.name);
                None
            }
            Err(why) => {
                tracing::error!("a built-in audio chain doesn't read: {why}");
                debug_assert!(false, "a built-in audio chain doesn't read: {why}");
                None
            }
        })
        .collect()
}

/// The clips recipes Klipp ships (#2886), as files in
/// `assets/recipes/cut`: each read and checked as a saved one is.
pub fn builtin_clip_recipes() -> Vec<CompositionTemplate> {
    const FILES: [&str; 3] = [
        include_str!("../../../assets/recipes/cut/track_matte_reveal.klipptemplate"),
        include_str!("../../../assets/recipes/cut/whip_pan_left.klipptemplate"),
        include_str!("../../../assets/recipes/cut/whip_pan_right.klipptemplate"),
    ];
    FILES
        .iter()
        .filter_map(|json| match CompositionTemplate::read(json) {
            Ok(t) if t.kind == TemplateKind::Clips => Some(t),
            Ok(t) => {
                debug_assert!(false, "\"{}\" is not a clips recipe", t.name);
                None
            }
            Err(why) => {
                tracing::error!("a built-in clips recipe doesn't read: {why}");
                debug_assert!(false, "a built-in clips recipe doesn't read: {why}");
                None
            }
        })
        .collect()
}

/// The recipes Klipp ships (#2758), as files in `assets/recipes`: each a
/// recipe like a saved one, read and checked as one is.
pub fn builtin_recipes() -> Vec<CompositionTemplate> {
    const FILES: [&str; 18] = [
        include_str!("../../../assets/recipes/green_screen.klipptemplate"),
        include_str!("../../../assets/recipes/picture_in_picture.klipptemplate"),
        include_str!("../../../assets/recipes/matted_glow.klipptemplate"),
        include_str!("../../../assets/recipes/sky.klipptemplate"),
        include_str!("../../../assets/recipes/text_behind_subject.klipptemplate"),
        include_str!("../../../assets/recipes/screen_replacement.klipptemplate"),
        include_str!("../../../assets/recipes/clean_plate_background.klipptemplate"),
        include_str!("../../../assets/recipes/colour_hold.klipptemplate"),
        include_str!("../../../assets/recipes/camera_shake.klipptemplate"),
        include_str!("../../../assets/recipes/glitch.klipptemplate"),
        include_str!("../../../assets/recipes/gradient_map.klipptemplate"),
        include_str!("../../../assets/recipes/wave.klipptemplate"),
        include_str!("../../../assets/recipes/twirl.klipptemplate"),
        include_str!("../../../assets/recipes/bulge.klipptemplate"),
        include_str!("../../../assets/recipes/redact_mosaic.klipptemplate"),
        include_str!("../../../assets/recipes/redact_blur.klipptemplate"),
        include_str!("../../../assets/recipes/zoom.klipptemplate"),
        include_str!("../../../assets/recipes/zoom_push.klipptemplate"),
    ];
    FILES
        .iter()
        .filter_map(|json| match CompositionTemplate::read(json) {
            Ok(t) => Some(t),
            Err(why) => {
                tracing::error!("a built-in recipe doesn't read: {why}");
                debug_assert!(false, "a built-in recipe doesn't read: {why}");
                None
            }
        })
        .collect()
}

/// The sequences of `template` that `project` lacks, by id, in the order
/// the template carries them: what inserting it copies in.
pub fn missing_sequences(template: &CompositionTemplate, project: &[Sequence]) -> Vec<Sequence> {
    template.sequences.iter().filter(|s| !project.iter().any(|p| p.id == s.id)).cloned().collect()
}

/// Every template file in `dir` that reads and checks, by name, a second
/// file with an id already read left out.
fn read_templates_in(dir: &Path) -> Vec<(PathBuf, CompositionTemplate)> {
    let mut entries: Vec<(PathBuf, CompositionTemplate)> = Vec::new();
    for path in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.path()) {
        if path.extension().and_then(|extension| extension.to_str()) != Some(EXTENSION) {
            continue;
        }
        match read_capped(&path).and_then(|json| CompositionTemplate::read(&json)) {
            Ok(template) => {
                // One id is one template: a second file carrying it is a
                // copy, and saving would rewrite the first while dropping
                // both from the list.
                if entries.iter().any(|(_, existing)| existing.id == template.id) {
                    tracing::warn!("Skipping template '{}': its id is already loaded from another file", path.display());
                } else {
                    entries.push((path, template));
                }
            }
            Err(error) => tracing::warn!("Skipping template '{}': {error}", path.display()),
        }
    }
    entries.sort_by_key(|(path, template)| (template.name.to_lowercase(), path.clone()));
    entries
}

#[derive(Debug, Default)]
pub struct CompositionTemplateStore {
    dir: Option<PathBuf>,
    entries: Vec<(PathBuf, CompositionTemplate)>,
    /// The templates Klipp ships, when asked for ([`Self::with_builtins`]).
    builtins: Vec<CompositionTemplate>,
    /// The installed packs' templates and recipes, by pack (#2757), when
    /// asked for ([`Self::with_packs`]): read-only.
    packs: Vec<(String, CompositionTemplate)>,
}

impl CompositionTemplateStore {
    /// The store with the templates Klipp ships beside the user's (#2757),
    /// listed after them and the packs'.
    pub fn with_builtins(mut self) -> Self {
        self.builtins = builtin_templates();
        self
    }

    /// The store with each pack installed under `root` (see
    /// [`crate::template_places::data_root`]) beside the user's (#2757):
    /// its templates and recipes from its `templates` folder, read-only.
    pub fn with_packs(mut self, root: &Path) -> Self {
        for (name, folder) in crate::template_places::installed(root) {
            self.packs.extend(read_templates_in(&folder.join("templates")).into_iter().map(|(_, t)| (name.clone(), t)));
        }
        self
    }

    /// Which source the template `id` is from (#2757): the user's own
    /// first, then a pack's, then Klipp's.
    pub fn source_of(&self, id: Uuid) -> crate::template_places::Source {
        use crate::template_places::Source;
        if self.entries.iter().any(|(_, t)| t.id == id) {
            Source::Mine
        } else if let Some((pack, _)) = self.packs.iter().find(|(_, t)| t.id == id) {
            Source::Pack(pack.clone())
        } else {
            Source::Builtin
        }
    }

    /// Whether `id` is a template Klipp ships, which is never written.
    pub fn is_builtin(&self, id: Uuid) -> bool {
        self.builtins.iter().any(|t| t.id == id) && self.source_of(id) == crate::template_places::Source::Builtin
    }

    /// The template or recipe called `name` (in any case), from `source`
    /// when one is given, else the user's own before a pack's before
    /// Klipp's (#2757): the same name may be in each.
    pub fn find(&self, name: &str, source: Option<&crate::template_places::Source>) -> Option<&CompositionTemplate> {
        self.templates()
            .chain(self.recipes())
            .chain(self.builtins.iter().filter(|t| t.kind == TemplateKind::Recipe))
            .filter(|t| t.name.eq_ignore_ascii_case(name.trim()))
            .map(|t| (self.source_of(t.id), t))
            .filter(|(s, _)| source.is_none_or(|want| s == want))
            .min_by_key(|(s, _)| s.rank())
            .map(|(_, t)| t)
    }

    /// The template folder under `config` (the config directory).
    pub fn default_dir(config: &Path) -> PathBuf {
        config.join("klipp").join("templates")
    }

    pub fn load(dir: Option<PathBuf>) -> Self {
        let entries = dir.as_deref().map(read_templates_in).unwrap_or_default();
        Self { dir, entries, builtins: Vec::new(), packs: Vec::new() }
    }

    /// The compositions: what is placed as a clip of its own. A recipe
    /// is never one (#2758).
    /// The user's, then the packs', then Klipp's (#2757), each once (by
    /// id): the same name may be in more than one, none hiding another.
    pub fn templates(&self) -> impl Iterator<Item = &CompositionTemplate> {
        let mine = self.entries.iter().map(|(_, t)| t);
        let packs = self.packs.iter().map(|(_, t)| t).filter(|t| !self.entries.iter().any(|(_, m)| m.id == t.id));
        let shipped = self.builtins.iter().filter(|b| !self.entries.iter().any(|(_, t)| t.id == b.id) && !self.packs.iter().any(|(_, t)| t.id == b.id));
        mine.chain(packs).chain(shipped).filter(|t| t.kind == TemplateKind::Composition)
    }

    /// The recipes saved here and in the installed packs (#2758);
    /// [`builtin_recipes`] are the ones Klipp ships.
    pub fn recipes(&self) -> impl Iterator<Item = &CompositionTemplate> {
        let packs = self.packs.iter().map(|(_, t)| t).filter(|t| !self.entries.iter().any(|(_, m)| m.id == t.id));
        self.entries.iter().map(|(_, template)| template).chain(packs).filter(|t| t.kind == TemplateKind::Recipe)
    }

    /// The clips recipes (#2886): the user's own, then the packs'.
    pub fn clip_recipes(&self) -> impl Iterator<Item = &CompositionTemplate> {
        let packs = self.packs.iter().map(|(_, t)| t).filter(|t| !self.entries.iter().any(|(_, m)| m.id == t.id));
        self.entries.iter().map(|(_, template)| template).chain(packs).filter(|t| t.kind == TemplateKind::Clips)
    }

    /// The audio chain recipes (#2886): the user's own, then the packs'.
    pub fn audio_chains(&self) -> impl Iterator<Item = &CompositionTemplate> {
        let packs = self.packs.iter().map(|(_, t)| t).filter(|t| !self.entries.iter().any(|(_, m)| m.id == t.id));
        self.entries.iter().map(|(_, template)| template).chain(packs).filter(|t| t.kind == TemplateKind::AudioChain)
    }

    pub fn save(&mut self, mut template: CompositionTemplate) -> Result<(), String> {
        // A pack's template comes from someone else's build (#2888).
        template.within_limits()?;
        template.migrate_ids()?;
        template.validate()?;
        // A shipped template or a pack's is never written: saved, it is a
        // copy of the user's own.
        let theirs = self.builtins.iter().any(|b| b.id == template.id) || self.packs.iter().any(|(_, p)| p.id == template.id);
        if theirs && !self.entries.iter().any(|(_, t)| t.id == template.id) {
            template.id = Uuid::new_v4();
        }
        let dir = self
            .dir
            .as_ref()
            .ok_or("no template directory configured")?;
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let path = self
            .entries
            .iter()
            .find(|(_, existing)| existing.id == template.id)
            .map(|(path, _)| path.clone())
            .unwrap_or_else(|| {
                // A new template named like another gets a file of its
                // own rather than the other's.
                let stem = safe_stem(&template.name);
                let taken = |p: &PathBuf| self.entries.iter().any(|(existing, _)| existing == p);
                let mut candidate = dir.join(format!("{stem}.{EXTENSION}"));
                let mut n = 2;
                while taken(&candidate) || candidate.exists() {
                    candidate = dir.join(format!("{stem}-{n}.{EXTENSION}"));
                    n += 1;
                }
                candidate
            });
        let json = serde_json::to_string_pretty(&template).map_err(|error| error.to_string())?;
        std::fs::write(&path, json).map_err(|error| error.to_string())?;
        self.entries
            .retain(|(_, existing)| existing.id != template.id);
        self.entries.push((path, template));
        self.entries
            .sort_by_key(|(path, item)| (item.name.to_lowercase(), path.clone()));
        Ok(())
    }
}

/// A template file's text (#2758): a regular file, not a link to one
/// elsewhere or a device, of at most [`MAX_FILE_BYTES`], read no further
/// than that whatever it grows to meanwhile.
fn read_capped(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{} bytes, over the {MAX_FILE_BYTES} a template may be", meta.len()));
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_string(&mut text))
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("over the {MAX_FILE_BYTES} bytes a template may be"));
    }
    Ok(text)
}

fn safe_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        "template".into()
    } else {
        stem
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Clip, ClipKind};
    use klipp_core::RationalTime;

    fn tc(s: f64) -> klipp_core::Timecode {
        RationalTime::from_secs_f64(s, 24)
    }

    /// A composition nesting a precomposition that nests a compound: the
    /// three, and nothing else in the project.
    fn nested() -> (Sequence, Vec<Sequence>) {
        let inner = Sequence::new_default();
        let mut middle = Sequence::new_default();
        middle.tracks[0].clips.push(Clip { kind: ClipKind::Sequence { sequence_id: inner.id }, ..Clip::gap(tc(0.0), tc(2.0)) });
        let mut root = Sequence::new_default();
        root.tracks[0].clips.push(Clip::precomposition(middle.id, tc(0.0), tc(2.0)));
        let unrelated = Sequence::new_default();
        let all = vec![unrelated, inner, middle, root.clone()];
        (root, all)
    }

    /// #2757: a template carries its composition and every sequence it
    /// nests, at any depth; inserting it copies in those a project lacks.
    #[test]
    fn a_template_carries_what_its_composition_nests() {
        let (root, all) = nested();
        let carried = composition_sequences(&root, &all);
        assert_eq!(carried.len(), 3, "the composition, its precomposition and the compound inside that");
        assert_eq!(carried[0].id, root.id);
        assert!(!carried.iter().any(|s| s.id == all[0].id), "nothing it does not nest");
        let mut t = CompositionTemplate::new("Card", root.id.0);
        t.sequences = carried;
        t.validate().unwrap();
        assert_eq!(missing_sequences(&t, &all).len(), 0, "the project it came from has them all");
        assert_eq!(missing_sequences(&t, &[Sequence::new_default()]).len(), 3, "another project has none of them");
    }

    /// A template of format 1 only named its composition, and one of a
    /// newer format was written by a newer Klipp: both refused, as is one
    /// that does not carry its composition.
    #[test]
    fn a_template_without_its_composition_or_of_another_format_is_refused() {
        let (root, all) = nested();
        let mut t = CompositionTemplate::new("Card", root.id.0);
        assert!(t.validate().unwrap_err().contains("carry its composition"));
        t.sequences = composition_sequences(&root, &all);
        t.format_version = 1;
        assert!(t.validate().unwrap_err().contains("save it again"));
        t.format_version = FORMAT_VERSION + 1;
        assert!(t.validate().unwrap_err().contains("newer Klipp"));
    }

    /// Saved, a template reads back carrying its sequences.
    #[test]
    fn a_saved_template_reads_back_with_its_composition() {
        let dir = std::env::temp_dir().join(format!("klipp-composition-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, all) = nested();
        let mut t = CompositionTemplate::new("Card", root.id.0);
        t.sequences = composition_sequences(&root, &all);
        let mut store = CompositionTemplateStore::load(Some(dir.clone()));
        store.save(t.clone()).unwrap();
        let read = CompositionTemplateStore::load(Some(dir.clone()));
        let back = read.templates().next().expect("read back");
        assert_eq!(serde_json::to_value(back).unwrap(), serde_json::to_value(&t).unwrap());
        assert_eq!(back.composition().map(|s| s.id), Some(root.id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scene of a title (a text and a filled shape) and a media clip.
    fn card_and_photo() -> (Sequence, klipp_core::ClipId, klipp_core::ClipId) {
        let mut title = crate::TitleData::single_text(crate::TextBox { text: "Launch".into(), color: [255, 255, 255, 255], ..Default::default() });
        let at = title.objects[0].transform.clone();
        title.add_object(crate::TitleObjectContent::Shape(crate::Shape { kind: crate::ShapeKind::Rectangle { corner_radius: 0.0 }, fit: None, fill: Some([200, 0, 0, 255]), gradient: None, stroke: None }), at);
        let mut seq = Sequence::new_default();
        let text = Clip { kind: ClipKind::Title(title), ..Clip::gap(tc(2.0), tc(5.0)) };
        let photo = Clip::media(klipp_core::MediaId::new(), tc(2.0), tc(5.0), tc(0.0), tc(3.0));
        let (t, p) = (text.id, photo.id);
        seq.tracks[0].clips.push(photo);
        let mut top = crate::Track::new("V2", crate::TrackKind::Video);
        top.clips.push(text);
        seq.tracks.insert(0, top);
        (seq, t, p)
    }

    /// #2757: clips saved together keep their tracks, start at 0 and
    /// leave the timeline as it was; the scene offers each text, colour
    /// and font, the shape's colour and the media clip as a slot, its
    /// texts and slots ticked; an unknown label is refused.
    #[test]
    fn clips_saved_together_offer_their_texts_colours_and_slots() {
        let (seq, t, p) = card_and_photo();
        let before = serde_json::to_value(&seq).unwrap();
        let scene = scene_from_clips(&seq, &[t, p]).expect("a scene");
        assert_eq!(serde_json::to_value(&seq).unwrap(), before, "the timeline untouched");
        assert_eq!(scene.tracks.len(), 2);
        assert!(scene.tracks.iter().flat_map(|t| &t.clips).all(|c| c.timeline_in == tc(0.0)), "from the first one's start");
        let labels: Vec<String> = candidates(&scene).into_iter().map(|c| c.label).collect();
        assert_eq!(labels, ["Text 1", "Text 1 colour", "Text 1 font", "Shape 2 colour", "Media 1"]);
        let t = template_from_scene(&scene, &[], "Card", None).unwrap();
        assert_eq!(t.parameters.values().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["Text 1"], "texts by default");
        assert_eq!(t.slots.values().map(|s| (s.label.as_str(), s.kind)).collect::<Vec<_>>(), [("Media 1", SlotKind::Media)]);
        assert!(template_from_scene(&scene, &[], "Card", Some(&["Logo".into()])).unwrap_err().contains("they have Text 1"));
    }

    /// Placed, an empty slot shows the placeholder and a filled one its
    /// media; a colour and a font set by their parameters land on their
    /// layers.
    #[test]
    fn a_placed_template_fills_its_slots_and_sets_its_properties() {
        let (seq, t, p) = card_and_photo();
        let scene = scene_from_clips(&seq, &[t, p]).unwrap();
        let chosen: Vec<String> = ["Text 1", "Shape 2 colour", "Text 1 font", "Media 1"].map(String::from).to_vec();
        let template = template_from_scene(&scene, &[], "Card", Some(&chosen)).unwrap();
        let place = |overrides: Vec<(&str, klipp_core::TemplateValue)>, media: Option<klipp_core::MediaId>| {
            let instance = klipp_core::TemplateInstance {
                template_id: template.id,
                bindings: template.bindings.iter().map(|b| (b.parameter.clone(), b.property.clone())).collect(),
                overrides: overrides.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                media_slots: media.map(|m| ("slot_1".to_string(), m.0.to_string())).into_iter().collect(),
                links: Vec::new(),
            };
            let mut clip = Clip::precomposition(scene.id, tc(0.0), tc(3.0));
            clip.kind = ClipKind::TemplatePrecomposition { sequence_id: scene.id, template_instance: instance };
            let mut outer = Sequence::new_default();
            outer.tracks[0].clips.push(clip);
            let flat = crate::flatten::flatten_sequence(&outer, &[outer.clone(), scene.clone()]);
            let inner = flat.all_clips().find_map(|c| c.precomp.clone()).expect("the scene");
            (*inner).clone()
        };
        let empty = place(vec![], None);
        assert!(empty.all_clips().any(|c| matches!(c.kind, ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color }) if color == crate::flatten::SLOT_PLACEHOLDER)), "the placeholder where no media was given");
        let media = klipp_core::MediaId::new();
        let filled = place(vec![("color_1", klipp_core::TemplateValue::Color([0.0, 0.0, 1.0, 1.0])), ("font_0", klipp_core::TemplateValue::Text("Anton".into()))], Some(media));
        assert!(filled.all_clips().any(|c| matches!(c.kind, ClipKind::Media { media_id } if media_id == media)), "the slot's media");
        let title = filled.all_clips().find_map(|c| c.title_data().cloned()).unwrap();
        assert!(matches!(&title.objects[0].content, crate::TitleObjectContent::TextBox(tb) if tb.font_name == "Anton"));
        assert!(matches!(&title.objects[1].content, crate::TitleObjectContent::Shape(s) if s.fill == Some([0, 0, 255, 255])));
    }

    /// #2757: a control drives each target along its range and curve;
    /// a hand edit becomes an offset the control keeps adding; unlink
    /// freezes, relink does not jump, reset snaps back. The template file
    /// and the placed instance keep all of it; a node target is refused,
    /// naming the Composite page.
    #[test]
    fn a_macro_control_drives_its_targets_and_keeps_hand_edits() {
        let (seq, t, p) = card_and_photo();
        let scene = scene_from_clips(&seq, &[t, p]).unwrap();
        let mut template = template_from_scene(&scene, &[], "Card", None).unwrap();
        let controls: Vec<ControlSpec> = serde_json::from_value(serde_json::json!([{ "label": "Grow", "drives": [
            { "target": "Text 1 size", "to": [40.0, 80.0] },
            { "target": "text 1 opacity", "to": [0.0, 1.0], "curve": "ease_in" },
        ] }])).unwrap();
        add_controls(&mut template, &scene, &controls).unwrap();
        // The template file keeps the control and its links.
        let back: CompositionTemplate = serde_json::from_str(&serde_json::to_string(&template).unwrap()).unwrap();
        assert_eq!(back.bindings, template.bindings);
        assert_eq!(back.parameters["grow"].label, "Grow");
        let refused = add_controls(&mut template.clone(), &scene, &serde_json::from_value::<Vec<ControlSpec>>(serde_json::json!([{ "label": "Spin", "drives": [{ "target": "Logo size", "to": [0, 1] }] }])).unwrap()).unwrap_err();
        assert!(refused.contains("\"Spin\" drives \"Logo size\"") && refused.contains("Text 1 size"), "{refused}");

        let read = |instance: &klipp_core::TemplateInstance| {
            let mut clip = Clip::precomposition(scene.id, tc(0.0), tc(3.0));
            clip.kind = ClipKind::TemplatePrecomposition { sequence_id: scene.id, template_instance: instance.clone() };
            let mut outer = Sequence::new_default();
            outer.tracks[0].clips.push(clip);
            let flat = crate::flatten::flatten_sequence(&outer, &[outer.clone(), scene.clone()]);
            let inner = flat.all_clips().find_map(|c| c.precomp.clone()).expect("the scene");
            (crate::flatten::linked_value(&inner, "object:0.size").unwrap(), crate::flatten::linked_value(&inner, "object:0.opacity").unwrap())
        };
        let mut instance = instance_for(&template);
        // Survives the project file as clip data.
        assert_eq!(serde_json::from_str::<klipp_core::TemplateInstance>(&serde_json::to_string(&instance).unwrap()).unwrap(), instance);
        let at = |instance: &mut klipp_core::TemplateInstance, v: f64| { instance.overrides.insert("grow".into(), klipp_core::TemplateValue::Number(v)); read(instance) };
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4;
        assert!(close(at(&mut instance, 0.0), (40.0, 0.0)), "the bottom of each range");
        assert!(close(at(&mut instance, 0.5), (60.0, 0.25)), "linear halfway; ease in a quarter: {:?}", read(&instance));
        assert!(close(at(&mut instance, 1.0), (80.0, 1.0)), "the top of each range");
        // A hand edit at 1: size 90 is an offset of +10 the control keeps.
        instance.links[0].set_value(1.0, 90.0);
        assert!(close(at(&mut instance, 0.5), (70.0, 0.25)), "the offset rides on the control");
        // Unlinked at 0.5 it stays at 70 wherever the control goes.
        instance.links[0].unlink(0.5);
        assert!(close(at(&mut instance, 0.0), (70.0, 0.0)), "frozen");
        // Relinked at 0: no jump, then it follows with offset +30.
        instance.links[0].relink(0.0);
        assert!(close(read(&instance), (70.0, 0.0)), "relinking does not jump");
        assert!(close(at(&mut instance, 1.0), (110.0, 1.0)));
        instance.links[0].reset_offset();
        assert!(close(read(&instance), (80.0, 1.0)), "reset snaps to the control");

        instance.links[0].property = "node:grade/lift.gain".into();
        let err = crate::flatten::validate_template_instance(&instance, &scene).unwrap_err();
        assert!(err.contains("recipe's node parameter") && err.contains("composition template cannot bind one"), "{err}");
    }

    /// A recipe (#2758) is a template file of its own kind: the store
    /// lists it apart from the compositions, it validates without a
    /// composition but with its graph, its bindings name an op that adds a
    /// node, and placing it resolves its anchors and new nodes to paths on
    /// the clip, with a slot left unfilled refused by name.
    #[test]
    fn a_recipe_is_a_template_of_its_own_kind() {
        let recipes = builtin_recipes();
        let r = recipes.iter().find(|r| r.name == "Picture in picture").expect("a built-in");
        assert!(r.validate().is_ok());
        assert_eq!(node_binding("node:3.scale_x"), Some((3, "scale_x")));
        assert_eq!(behaviour_binding("node:3.position_x~amplitude"), Some((3, "position_x", "amplitude")));
        assert_eq!(behaviour_binding("node:3.position_x"), None);
        assert_eq!(node_binding("node:x.scale_x"), None);
        assert_eq!(node_binding("node:3."), None);

        let mut bad = r.clone();
        bad.bindings[0].property = "node:2.media".into();
        assert!(bad.validate().unwrap_err().contains("adds no node"), "op 2 connects");
        let mut bare = r.clone();
        bare.graph = None;
        assert!(bare.validate().is_err(), "a recipe carries its graph");

        let dir = std::env::temp_dir().join(format!("klipp-recipe-kind-{}", Uuid::new_v4()));
        let mut store = CompositionTemplateStore::load(Some(dir.clone()));
        store.save(r.clone()).unwrap();
        let read = CompositionTemplateStore::load(Some(dir.clone()));
        assert_eq!(read.templates().count(), 0, "a recipe is never placed as a composition");
        assert_eq!(read.recipes().map(|t| t.id).collect::<Vec<_>>(), vec![r.id]);
        std::fs::remove_dir_all(&dir).ok();

        let clip = Uuid::new_v4();
        let anchors = BTreeMap::from([("source", Uuid::new_v4()), ("end", Uuid::new_v4())]);
        let frame = klipp_core::Resolution { width: 1000, height: 500 };
        let why = r.recipe_ops(clip, &anchors, &BTreeMap::new(), &Placing::on_frame(frame)).unwrap_err();
        assert!(why.contains("fill the slot 'picture'"), "{why}");
        let values = BTreeMap::from([("picture".to_string(), TemplateValue::Text("m".into())), ("across".to_string(), TemplateValue::Number(1.0))]);
        let ops = r.recipe_ops(clip, &anchors, &values, &Placing::on_frame(frame)).unwrap();
        assert_eq!(ops[0]["after"], serde_json::json!({ "root": { "clip": clip }, "nodes": [anchors["end"]] }));
        assert_eq!(ops[2]["to"], serde_json::json!({ "root": { "clip": clip }, "nodes": [{ "new": 1 }] }));
        assert_eq!(ops[1]["params"]["media"], "m");
        // Across at its end: half the frame's width right, in its pixels.
        assert_eq!(ops[3]["params"]["position_x"], 500.0);
    }

    /// A colour's hue taken by a binding (#2758): pure red, green and blue
    /// land at 0, 120 and 240 degrees, a picked colour where the
    /// qualifier's picker puts it; only a colour gives one. A recipe binding
    /// it onto a qualifier's hue centre writes that hue, and a hue taken
    /// from a number control is refused when the recipe is read.
    #[test]
    fn a_binding_takes_a_colours_hue_as_the_picker_reads_it() {
        let hue = |c: [f32; 3]| match taken(&TemplateValue::Color([c[0], c[1], c[2], 1.0]), Some(Take::Hue)).unwrap() {
            TemplateValue::Number(h) => h,
            other => panic!("a number: {other:?}"),
        };
        assert!(hue([1.0, 0.0, 0.0]).abs() < 0.5 || (hue([1.0, 0.0, 0.0]) - 360.0).abs() < 0.5);
        assert!((hue([0.0, 1.0, 0.0]) - 120.0).abs() < 0.5);
        assert!((hue([0.0, 0.0, 1.0]) - 240.0).abs() < 0.5);
        let orange = [0.9, 0.5, 0.1];
        let picked = klipp_effects::cpu_grade::picture_hsl(orange.map(klipp_color::transfer::srgb_to_linear)).h * 360.0;
        assert!((hue(orange) - f64::from(picked)).abs() < 1e-3, "as the picker reads it");
        assert!(taken(&TemplateValue::Number(3.0), Some(Take::Hue)).is_err());

        let recipe = |default: serde_json::Value| -> CompositionTemplate {
            serde_json::from_value(serde_json::json!({
                "format_version": 2, "id": "6d1f3a92-8b4c-4e57-a0d3-2c9e7b1f5a68", "name": "Hold", "kind": "recipe",
                "composition_id": "00000000-0000-0000-0000-000000000000",
                "parameters": { "colour": { "label": "Colour", "default": default } },
                "bindings": [{ "parameter": "colour", "property": "node:0.qual_hue_center", "take": "hue" }],
                "graph": { "ops": [{ "op": "add_beside", "at": { "anchor": "source" }, "type": "klipp.grade.qualifier" }] }
            }))
            .unwrap()
        };
        let r = recipe(serde_json::json!({ "type": "Color", "value": [0.0, 0.0, 1.0, 1.0] }));
        r.validate().unwrap();
        let anchors = BTreeMap::from([("source", Uuid::new_v4())]);
        let ops = r.recipe_ops(Uuid::new_v4(), &anchors, &BTreeMap::new(), &Placing::on_frame(klipp_core::Resolution { width: 100, height: 100 })).unwrap();
        assert!((ops[0]["params"]["qual_hue_center"].as_f64().unwrap() - 240.0).abs() < 0.5, "blue's hue set: {}", ops[0]["params"]);
        let why = recipe(serde_json::json!({ "type": "Number", "value": 1.0 })).validate().unwrap_err();
        assert!(why.contains("takes a hue"), "{why}");
    }

    /// A control sets one value inside a parameter (#2886): a mask's
    /// rectangle placed and sized by numbers, the rest of the mask as the
    /// recipe wrote it, linked through a range like any number; a pointer
    /// to nothing is refused when the recipe is read.
    #[test]
    fn a_control_sets_a_value_inside_a_parameter() {
        let recipe = |pointer: &str| -> CompositionTemplate {
            serde_json::from_value(serde_json::json!({
                "format_version": 2, "id": "2b7c9e14-5a3d-4f86-9c21-7e4b0d8a6f13", "name": "Window", "kind": "recipe",
                "composition_id": "00000000-0000-0000-0000-000000000000",
                "parameters": {
                    "x": { "label": "Across", "default": { "type": "Number", "value": 0.25 }, "range": { "min": 0.0, "max": 1.0 } },
                    "w": { "label": "Width", "default": { "type": "Number", "value": 0.5 }, "range": { "min": 0.0, "max": 1.0 } }
                },
                "bindings": [
                    { "parameter": "x", "property": format!("node:0.masks/masks/0/shape/Rectangle/{pointer}") },
                    { "parameter": "w", "property": "node:0.masks/masks/0/shape/Rectangle/width", "link": { "to": [0.0, 2.0] } }
                ],
                "graph": { "ops": [{ "op": "add_beside", "at": { "anchor": "source" }, "type": "klipp.grade.mask",
                    "params": { "masks": { "masks": [{ "shape": { "Rectangle": { "center_x": 0.5, "center_y": 0.5, "width": 0.3, "height": 0.6, "rotation": 0 } }, "feather": 0, "opacity": 1, "invert": false, "enabled": true }] } } }] }
            }))
            .unwrap()
        };
        let r = recipe("center_x");
        r.validate().unwrap();
        let anchors = BTreeMap::from([("source", Uuid::new_v4())]);
        let values = BTreeMap::from([("x".to_string(), TemplateValue::Number(0.75))]);
        let ops = r.recipe_ops(Uuid::new_v4(), &anchors, &values, &Placing::on_frame(klipp_core::Resolution { width: 100, height: 100 })).unwrap();
        let rect = &ops[0]["params"]["masks"]["masks"][0]["shape"]["Rectangle"];
        assert_eq!(rect["center_x"], 0.75);
        assert_eq!(rect["width"], 1.0, "0.5 through the link's 0..2");
        assert_eq!(rect["height"], 0.6, "the rest as written");
        assert!(ops[0]["params"].get("masks/masks/0/shape/Rectangle/center_x").is_none(), "set inside, not as a key of its own");
        let why = recipe("centre_x").validate().unwrap_err();
        assert!(why.contains("has nothing at /masks/0/shape/Rectangle/centre_x"), "{why}");
    }

    /// A recipe's keys (#2886): a control sets one key's value and a
    /// linked one its time; a binding to a key no keys op puts there is
    /// refused, and so are more keys than a parameter takes, or a key with
    /// no number for its time, when the recipe is read.
    #[test]
    fn a_control_sets_a_key_and_a_recipe_holds_few_finite_keys() {
        let recipe = |key: &str, keys: serde_json::Value| -> CompositionTemplate {
            serde_json::from_value(serde_json::json!({
                "format_version": 2, "id": "3f8b2c61-9a4e-4d17-b5c3-6e2a0f9d7b14", "name": "Blur in", "kind": "recipe",
                "composition_id": "00000000-0000-0000-0000-000000000000",
                "parameters": {
                    "most": { "label": "Most", "default": { "type": "Number", "value": 8.0 }, "range": { "min": 0.0, "max": 40.0 } },
                    "when": { "label": "When", "default": { "type": "Number", "value": 0.5 }, "range": { "min": 0.0, "max": 1.0 } }
                },
                "bindings": [
                    { "parameter": "most", "property": format!("node:0.radius#{key}.value") },
                    { "parameter": "when", "property": "node:0.radius#1.time_secs", "link": { "to": [0.0, 4.0] } }
                ],
                "graph": { "ops": [
                    { "op": "add", "after": { "anchor": "end" }, "type": "klipp.fx.gaussian_blur" },
                    { "op": "keys", "path": { "new": 0 }, "key": "radius", "keys": keys }
                ] }
            }))
            .unwrap()
        };
        let two = serde_json::json!([{ "time_secs": 0.0, "value": 0.0 }, { "time_secs": 1.0, "value": 0.0 }]);
        let r = recipe("1", two.clone());
        r.validate().unwrap();
        let anchors = BTreeMap::from([("end", Uuid::new_v4())]);
        let values = BTreeMap::from([("most".to_string(), TemplateValue::Number(12.0))]);
        let ops = r.recipe_ops(Uuid::new_v4(), &anchors, &values, &Placing::on_frame(klipp_core::Resolution { width: 100, height: 100 })).unwrap();
        assert_eq!(ops[1]["keys"][1]["value"], 12.0);
        assert_eq!(ops[1]["keys"][1]["time_secs"], 2.0, "0.5 through the link's 0..4");
        assert_eq!(ops[1]["keys"][0], serde_json::json!({ "time_secs": 0.0, "value": 0.0 }), "the other key as written");
        let why = recipe("5", two).validate().unwrap_err();
        assert!(why.contains("no key 5"), "{why}");
        let many: Vec<serde_json::Value> = (0..=MAX_RECIPE_KEYS).map(|i| serde_json::json!({ "time_secs": i as f64, "value": 0.0 })).collect();
        let why = CompositionTemplate::read(&serde_json::to_string(&recipe("1", serde_json::json!(many))).unwrap()).unwrap_err();
        assert!(why.contains("at most 256 keys"), "{why}");
        let why = CompositionTemplate::read(&serde_json::to_string(&recipe("1", serde_json::json!([{ "time_secs": "soon", "value": 1.0 }, { "time_secs": 1.0, "value": 0.0 }]))).unwrap()).unwrap_err();
        assert!(why.contains("finite time_secs and value"), "{why}");
    }

    /// A template folder is somewhere anyone can drop a file (#2758): what
    /// isn't a template made here is skipped or refused, said, never read
    /// whole or built from. An oversized file, a link to a file elsewhere
    /// and JSON nested past reason are skipped; a recipe with more ops
    /// than a graph batch takes, a name past its length or with control
    /// characters is refused; a name shaped like a path saves inside the
    /// folder all the same.
    #[test]
    fn a_hostile_template_file_is_skipped_or_refused() {
        let dir = std::env::temp_dir().join(format!("klipp-hostile-templates-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = builtin_recipes().into_iter().next().expect("a built-in");
        let json = serde_json::to_string(&good).unwrap();
        // Over the cap: padded past it with spaces, still valid JSON.
        let mut big = json.clone();
        big.push_str(&" ".repeat(MAX_FILE_BYTES as usize));
        std::fs::write(dir.join(format!("big.{EXTENSION}")), &big).unwrap();
        // A link to a template outside the folder.
        let outside = std::env::temp_dir().join(format!("klipp-outside-{}.{EXTENSION}", Uuid::new_v4()));
        std::fs::write(&outside, &json).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join(format!("link.{EXTENSION}"))).unwrap();
        // Nested past reason.
        std::fs::write(dir.join(format!("deep.{EXTENSION}")), "[".repeat(100_000)).unwrap();
        let store = CompositionTemplateStore::load(Some(dir.clone()));
        assert_eq!(store.recipes().count() + store.templates().count(), 0, "none of them is read");

        let mut many = good.clone();
        many.graph.as_mut().unwrap().ops = vec![serde_json::json!({"op": "add", "after": {"anchor": "end"}, "type": "klipp.fx.invert"}); MAX_RECIPE_OPS + 1];
        assert!(many.validate().unwrap_err().contains("graph ops"));
        let mut long = good.clone();
        long.name = "x".repeat(MAX_TEXT + 1);
        assert!(long.validate().is_err());
        let mut control = good.clone();
        control.name = "Green\u{0}screen".into();
        assert!(control.validate().is_err());

        // A name shaped like a path is saved in the folder, by a plain stem.
        let mut store = CompositionTemplateStore::load(Some(dir.clone()));
        let mut away = good.clone();
        away.id = Uuid::new_v4();
        away.name = "../../escaped".into();
        store.save(away).unwrap();
        assert!(!dir.parent().unwrap().join("escaped.klipptemplate").exists());
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(&format!("escaped.{EXTENSION}"))), "saved inside the folder");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_file(&outside).ok();
    }

    /// #2886: a clips recipe from a file is checked before it is used:
    /// its slots clips, those it finds its own; each step on the placed
    /// clip or a slot, a matte seen through another slot with a known
    /// channel and finite, non-negative feather and choke, or keys on one
    /// of the clip properties it may key, 1 to 64 of them at finite times
    /// with finite values; 1 to 32 steps; bindings to a key there is, from
    /// a number. Each is refused; one in a person's folder is left out.
    #[test]
    fn a_hostile_clips_recipe_is_refused() {
        let reveal = || builtin_clip_recipes().into_iter().find(|r| r.name == "Track Matte Reveal").expect("shipped");
        let whip = || builtin_clip_recipes().into_iter().find(|r| r.name == "Whip Pan (left)").expect("shipped");
        let refused = |base: CompositionTemplate, f: &dyn Fn(&mut CompositionTemplate)| {
            let mut t = base;
            f(&mut t);
            CompositionTemplate::read(&serde_json::to_string(&t).unwrap()).unwrap_err()
        };
        fn steps(t: &mut CompositionTemplate) -> &mut Vec<ClipStep> {
            &mut t.clips.as_mut().unwrap().steps
        }
        assert!(refused(reveal(), &|t| t.slots.get_mut("above").unwrap().kind = SlotKind::Media).contains("slots are clips"));
        assert!(refused(reveal(), &|t| { t.clips.as_mut().unwrap().find.insert("elsewhere".into(), ClipFind::Above); }).contains("not one of its slots"));
        assert!(refused(reveal(), &|t| steps(t)[0].clip = "other".into()).contains("neither placed nor one of its slots"));
        assert!(refused(reveal(), &|t| steps(t)[0].matte.as_mut().unwrap().source = "placed".into()).contains("not another of its slots"));
        assert!(refused(reveal(), &|t| steps(t)[0].matte.as_mut().unwrap().channel = Some("red".into())).contains("auto, alpha or luma"));
        assert!(refused(reveal(), &|t| steps(t)[0].matte.as_mut().unwrap().feather_px = -1.0).contains("finite pixels"));
        assert!(refused(reveal(), &|t| steps(t)[0].keys = Some(KeysStep { param: "opacity".into(), keys: vec![ClipKey { at: KeyTime::Secs(0.0), value: KeyValue::Number(1.0) }] })).contains("a matte or keys, one of them"));
        assert!(refused(reveal(), &|t| { steps(t)[1].keys = None; }).contains("a matte or keys, one of them"));
        assert!(refused(whip(), &|t| steps(t)[0].keys.as_mut().unwrap().param = "volume".into()).contains("it keys opacity, position_x"));
        assert!(refused(whip(), &|t| steps(t)[0].keys.as_mut().unwrap().keys.clear()).contains("from 1 to 64"));
        assert!(refused(whip(), &|t| { let k = &mut steps(t)[0].keys.as_mut().unwrap().keys; *k = vec![k[0].clone(); MAX_CLIP_KEYS + 1]; }).contains("at most 64"));
        assert!(refused(whip(), &|t| steps(t)[0].keys.as_mut().unwrap().keys[0].at = KeyTime::Secs(-1.0)).contains("finite times"));
        assert!(refused(whip(), &|t| steps(t)[0].keys.as_mut().unwrap().keys[0].at = KeyTime::FromEnd { from_end: -0.5 }).contains("finite times"));
        assert!(refused(whip(), &|t| { let s = steps(t); *s = vec![s[0].clone(); MAX_CLIP_STEPS + 1]; }).contains("at most 32"));
        assert!(refused(whip(), &|t| steps(t).clear()).contains("carries the steps"));
        assert!(refused(whip(), &|t| t.clips = None).contains("carries the steps"));
        let bind = |t: &mut CompositionTemplate, property: &str| {
            t.parameters.insert("amount".into(), TemplateParameter { label: "Amount".into(), default: TemplateValue::Number(1.0), range: None, choices: Vec::new() });
            t.bindings = vec![TemplateBinding { parameter: "amount".into(), property: property.into(), link: None, take: None }];
        };
        assert!(refused(whip(), &|t| bind(t, "steps.0.keys.7.value")).contains("names no key 7"));
        assert!(refused(whip(), &|t| bind(t, "node:0.amount")).contains("binds steps.<i>.keys.<j>.value|at"));
        assert!(refused(whip(), &|t| { bind(t, "steps.0.keys.1.value"); t.bindings[0].link = Some(LinkMap { to: [0.0, 1.0], curve: klipp_core::LinkCurve::Linear, per: None }); }).contains("bound as it is"));
        let mut ok = whip();
        bind(&mut ok, "steps.0.keys.1.value");
        CompositionTemplate::read(&serde_json::to_string(&ok).unwrap()).unwrap();
        // Too large a value doesn't read at all.
        let mut json = serde_json::to_value(whip()).unwrap();
        json["clips"]["steps"][0]["keys"]["keys"][0]["value"] = serde_json::json!(1e300);
        assert!(CompositionTemplate::read(&json.to_string()).is_err());

        let dir = std::env::temp_dir().join(format!("klipp-hostile-clips-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bad = whip();
        bad.id = Uuid::new_v4();
        bad.name = "Reach".into();
        steps(&mut bad)[0].clip = "the editor's desktop".into();
        std::fs::write(dir.join("reach.klipptemplate"), serde_json::to_string(&bad).unwrap()).unwrap();
        let mut mine = whip();
        mine.id = Uuid::new_v4();
        mine.name = "My whip".into();
        std::fs::write(dir.join("mine.klipptemplate"), serde_json::to_string(&mine).unwrap()).unwrap();
        let store = CompositionTemplateStore::load(Some(dir.clone()));
        assert_eq!(store.clip_recipes().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["My whip"]);
        assert_eq!(store.templates().count() + store.recipes().count() + store.audio_chains().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #2886: an audio chain recipe from a file names only Klipp's own
    /// audio effects (never an LV2 plugin's URI or an OFX bundle), only
    /// their settings, with finite values and a mix from 0 to 1, at most
    /// 16 steps, each named once and not blank; its bindings set a step's
    /// setting from a number. Each such file is refused, and one in a
    /// person's template folder is left out of the store.
    #[test]
    fn a_hostile_audio_chain_is_refused() {
        let good = || builtin_audio_chains().into_iter().find(|r| r.name == "Clean Audio").expect("shipped");
        let refused = |f: &dyn Fn(&mut CompositionTemplate)| {
            let mut t = good();
            f(&mut t);
            CompositionTemplate::read(&serde_json::to_string(&t).unwrap()).unwrap_err()
        };
        fn step(t: &mut CompositionTemplate) -> &mut AudioStep {
            &mut t.audio.as_mut().unwrap().steps[0]
        }
        for plugin in ["lv2:http://gareus.org/oss/lv2/dpl#mono", "lv2:urn:gx_amp", "ofx:net.sf.openfx.Gain", "/usr/lib/lv2/x.lv2", "Voice_Isolation"] {
            let why = refused(&|t| step(t).effect = plugin.into());
            assert!(why.contains("is not one of Klipp's audio effects"), "{plugin}: {why}");
        }
        assert!(refused(&|t| { step(t).params.insert("gain".into(), 1.0); }).contains("has no setting \"gain\""));
        assert!(refused(&|t| step(t).wet_dry = 1.5).contains("mix 0 to 1"));
        assert!(refused(&|t| step(t).wet_dry = -0.1).contains("mix 0 to 1"));
        assert!(refused(&|t| { let s = t.audio.as_mut().unwrap(); s.steps = vec![s.steps[0].clone(); MAX_AUDIO_STEPS + 1].into_iter().enumerate().map(|(i, mut x)| { x.name = format!("s{i}"); x }).collect(); }).contains("at most 16"));
        assert!(refused(&|t| t.audio.as_mut().unwrap().steps.clear()).contains("carries the steps"));
        assert!(refused(&|t| t.audio = None).contains("carries the steps"));
        assert!(refused(&|t| step(t).name = " ".into()).contains("blank or twice"));
        assert!(refused(&|t| step(t).name = "WIND".into()).contains("blank or twice"));
        assert!(refused(&|t| t.bindings[0].property = "step:loud.reduction".into()).contains("names no step"));
        assert!(refused(&|t| t.bindings[0].property = "step:voice.gain".into()).contains("has no setting"));
        assert!(refused(&|t| t.bindings[0].property = "node:0.reduction".into()).contains("binds step:<name>.<param>"));
        assert!(refused(&|t| t.bindings[0].link = Some(LinkMap { to: [0.0, 1.0], curve: klipp_core::LinkCurve::Linear, per: None })).contains("bound as it is"));
        assert!(refused(&|t| step(t).says = "x".repeat(MAX_TEXT + 1)).contains("is refused"));
        // A setting too large for an f32 doesn't read at all.
        let mut json = serde_json::to_value(good()).unwrap();
        json["audio"]["steps"][0]["params"]["reduction"] = serde_json::Value::Number(serde_json::Number::from_f64(1e300).unwrap());
        let why = CompositionTemplate::read(&json.to_string()).unwrap_err();
        assert!(why.contains("number out of range"), "{why}");

        // In a person's folder: left out, and a good one beside it read.
        let dir = std::env::temp_dir().join(format!("klipp-hostile-chain-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bad = good();
        bad.id = Uuid::new_v4();
        bad.name = "Plugged".into();
        bad.audio.as_mut().unwrap().steps[0].effect = "lv2:urn:gx_amp".into();
        std::fs::write(dir.join("plugged.klipptemplate"), serde_json::to_string(&bad).unwrap()).unwrap();
        let mut mine = good();
        mine.id = Uuid::new_v4();
        mine.name = "My clean".into();
        std::fs::write(dir.join("mine.klipptemplate"), serde_json::to_string(&mine).unwrap()).unwrap();
        let store = CompositionTemplateStore::load(Some(dir.clone()));
        assert_eq!(store.audio_chains().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["My clean"]);
        assert_eq!(store.templates().count() + store.recipes().count(), 0, "an audio chain is neither a composition nor a picture recipe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #2886: an audio chain recipe's steps, those asked for in its
    /// order, each the effect with its settings in the effect's own order
    /// and a bound one from its control (the default when none is given);
    /// a step it hasn't got is refused with those it has.
    #[test]
    fn an_audio_chain_makes_its_steps() {
        let r = builtin_audio_chains().into_iter().find(|r| r.name == "Clean Audio").expect("shipped");
        let all = r.audio_steps(&BTreeMap::new(), None).unwrap();
        assert_eq!(all.iter().map(|(n, _, e)| (n.as_str(), e.effect_type.as_str())).collect::<Vec<_>>(), [("voice", "voice_isolation"), ("wind", "denoiser"), ("hiss", "denoiser"), ("hum", "hum_removal"), ("deess", "deesser")]);
        assert_eq!(all[0].2.params, [("reduction".to_string(), 24.0), ("mode".to_string(), 0.0)], "the effect's order, the control's default");
        let some = r.audio_steps(&BTreeMap::from([("reduction".to_string(), TemplateValue::Number(12.0))]), Some(&["DEESS".into(), "voice".into()])).unwrap();
        assert_eq!(some.iter().map(|(n, _, _)| n.as_str()).collect::<Vec<_>>(), ["voice", "deess"], "the recipe's order");
        assert_eq!(some[0].2.params[0], ("reduction".to_string(), 12.0));
        assert!(r.audio_steps(&BTreeMap::new(), Some(&["reverb".into()])).unwrap_err().contains("has no step \"reverb\""));
    }

    /// #2886: the Terminal Card ships as a file: a dark card, the command
    /// and a blinking cursor, in a wide, a tall and a square layout. The
    /// card enters and drifts on keys eased by their named curves, each
    /// segment a Bezier with real handles; the command types on a
    /// character at a time over the 0.34 s its speed names; and each of its
    /// five parameters is bound.
    #[test]
    fn the_terminal_card_ships_as_a_file() {
        let card = builtin_templates().into_iter().find(|t| t.name == "Terminal Card").expect("shipped");
        assert_eq!((card.category.as_str(), card.formats.keys().map(String::as_str).collect::<Vec<_>>()), ("Cards", vec!["square", "tall"]));
        let title = card.composition().and_then(|s| s.tracks.iter().flat_map(|t| &t.clips).find_map(|c| c.title_data().cloned())).expect("its title");
        use crate::TitleObjectContent as C;
        assert!(matches!((&title.objects[0].content, &title.objects[1].content, &title.objects[2].content), (C::Shape(_), C::TextBox(_), C::Shape(_))));
        assert!(title.objects[0].fx().motion.is_active());
        assert!(title.objects[2].fx().loops.iter().any(|l| matches!(l, crate::TitleLoop::Blink { .. })), "the cursor blinks");
        let keys = &title.objects[0].transform.center_x_keyframes;
        assert!(keys.len() >= 4);
        for pair in keys.windows(2) {
            assert_eq!(pair[0].interpolation, klipp_core::Interpolation::Bezier, "each segment is eased");
            assert!(pair[0].tangent_out.0 > 0.0 || pair[1].tangent_in.0 < 0.0, "with real handles");
        }
        assert_eq!(keys[0].tangent_in, (0.0, 0.0), "nothing precedes the first key");
        let enter = title.objects[1].fx().motion.enter.expect("an entrance");
        assert_eq!(enter.stagger, crate::StaggerUnit::Char);
        assert!((enter.stagger_secs * "$ find invoice.pdf".chars().count() as f32 - 0.34).abs() < 1e-5, "{}", enter.stagger_secs);
        let mut bound: Vec<&str> = card.bindings.iter().map(|b| b.parameter.as_str()).collect();
        bound.sort();
        assert_eq!(bound, ["accent", "command_text", "entry_direction", "float_amount", "typing_speed"]);
    }

    /// #2757: the templates Klipp ships load and check as saved ones do;
    /// each picks its layout for the frame it is placed in, a frame
    /// without one getting the main; they list after the user's, one of
    /// the same name hidden by the user's, and are never written.
    #[test]
    fn built_in_templates_load_pick_their_layout_and_are_never_written() {
        let shipped = builtin_templates();
        let split = shipped.iter().find(|t| t.name == "Split Screen").expect("the split screen ships");
        assert_eq!(split.formats.keys().map(String::as_str).collect::<Vec<_>>(), ["square", "tall"]);
        let id_of = |w, h| split.composition_for(w, h).map(|s| (s.id.0, s.resolution.width, s.resolution.height));
        assert_eq!(id_of(1920, 1080), Some((split.composition_id, 1920, 1080)));
        assert_eq!(id_of(3840, 2160).map(|t| t.0), Some(split.composition_id), "any wide frame: the main");
        assert_eq!(id_of(1080, 1920).map(|t| (t.1, t.2)), Some((1080, 1920)));
        assert_eq!(id_of(1080, 1080).map(|t| (t.1, t.2)), Some((1080, 1080)));
        let mut plain = split.clone();
        plain.formats.clear();
        assert_eq!(plain.composition_for(1080, 1920).map(|s| s.id.0), Some(split.composition_id), "no tall layout: the main");

        let dir = std::env::temp_dir().join(format!("klipp-builtins-{}", Uuid::new_v4()));
        let mut store = CompositionTemplateStore::load(Some(dir.clone())).with_builtins();
        assert!(store.templates().any(|t| t.name == "Split Screen") && store.is_builtin(split.id));
        // Saved over, it is the user's copy, under a new id, beside the
        // shipped one (#2757: a name may be in two sources), found first.
        store.save(split.clone()).unwrap();
        let reread = CompositionTemplateStore::load(Some(dir.clone())).with_builtins();
        let named: Vec<&CompositionTemplate> = reread.templates().filter(|t| t.name == "Split Screen").collect();
        assert_eq!(named.len(), 2, "the user's copy and the shipped one");
        let mine = reread.find("Split Screen", None).unwrap();
        assert!(mine.id != split.id && reread.source_of(mine.id) == crate::template_places::Source::Mine, "the user's copy first");
        assert_eq!(reread.find("split screen", Some(&crate::template_places::Source::Builtin)).unwrap().id, split.id);
        // A layout that is not carried, or a format not known, is refused.
        let mut bad = split.clone();
        bad.formats.insert("tall".into(), Uuid::new_v4());
        assert!(bad.validate().unwrap_err().contains("tall layout is not carried"));
        let mut bad = split.clone();
        bad.formats.insert("portrait".into(), split.composition_id);
        assert!(bad.validate().unwrap_err().contains("not tall or square"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #2888: a recipe naming renamed ids reads as the ids they are now,
    /// its type, a parameter in an add op, a modify op's key, a port and
    /// a binding (the parameter whole, or a value inside it by pointer, a
    /// key or a field); a removed type, an unknown one and renames that
    /// loop refuse it, saying which.
    #[test]
    fn a_recipe_reads_renamed_ids_as_they_are_now() {
        use crate::registry_ids::{Change, Migration, Subject};
        let table = [
            Migration { old: Subject::Type("klipp.fx.old_blur"), change: Change::Renamed("klipp.fx.gaussian_blur") },
            Migration { old: Subject::Param("klipp.fx.gaussian_blur", "old_radius"), change: Change::Renamed("radius") },
            Migration { old: Subject::Port("klipp.fx.merge", "front"), change: Change::Renamed("fg") },
            Migration { old: Subject::Type("klipp.fx.retired"), change: Change::Removed("drawn by gaussian_blur now") },
            Migration { old: Subject::Type("klipp.fx.ping"), change: Change::Renamed("klipp.fx.pong") },
            Migration { old: Subject::Type("klipp.fx.pong"), change: Change::Renamed("klipp.fx.ping") },
        ];
        let recipe = |ty: &str| -> CompositionTemplate {
            let mut t = builtin_recipes().remove(0);
            t.bindings = ["node:0.old_radius", "node:0.old_radius/inner/0", "node:0.old_radius#1.value", "node:0.old_radius~x"]
                .iter()
                .map(|p| serde_json::from_value(serde_json::json!({"parameter": "amount", "property": p})).unwrap())
                .collect();
            t.graph.as_mut().unwrap().ops = vec![
                serde_json::json!({"op": "add", "after": {"anchor": "end"}, "type": ty, "params": {"old_radius": 3.0}}),
                serde_json::json!({"op": "modify", "path": {"new": 0}, "key": "old_radius", "modifiers": []}),
                serde_json::json!({"op": "add", "after": {"new": 0}, "type": "klipp.fx.merge"}),
                serde_json::json!({"op": "connect", "from": {"anchor": "source"}, "from_port": "out", "to": {"new": 2}, "to_port": "front"}),
            ];
            t
        };
        let mut t = recipe("klipp.fx.old_blur");
        t.migrate_ids_in(&table).unwrap();
        let ops = &t.graph.as_ref().unwrap().ops;
        assert_eq!(ops[0]["type"], "klipp.fx.gaussian_blur");
        assert_eq!(ops[0]["params"], serde_json::json!({"radius": 3.0}));
        assert_eq!(ops[1]["key"], "radius");
        assert_eq!(ops[3]["to_port"], "fg");
        assert_eq!(ops[3]["from_port"], "out", "an anchor's port is the picture's, left as it is");
        let bound: Vec<&str> = t.bindings.iter().map(|b| b.property.as_str()).collect();
        assert_eq!(bound, ["node:0.radius", "node:0.radius/inner/0", "node:0.radius#1.value", "node:0.radius~x"], "a binding into the parameter too");
        for (ty, says) in [("klipp.fx.retired", "was removed: drawn by gaussian_blur now"), ("klipp.fx.never_was", "is not an effect or node"), ("klipp.fx.ping", "loop")] {
            let why = recipe(ty).migrate_ids_in(&table).unwrap_err();
            assert!(why.contains(says) && why.contains(ty), "{ty}: {why}");
        }
    }

    /// A recipe that keys with the chroma key's first name for its edge
    /// softness, `spill_suppression`, reads it as `spill` through the
    /// table Klipp ships, as a project does by the field's serde alias.
    #[test]
    fn a_recipe_reads_the_chroma_keys_old_edge_name() {
        let mut t = builtin_recipes().remove(0);
        t.bindings = vec![serde_json::from_value(serde_json::json!({"parameter": "amount", "property": "node:0.spill_suppression"})).unwrap()];
        t.graph.as_mut().unwrap().ops = vec![serde_json::json!({"op": "add", "after": {"anchor": "end"}, "type": "klipp.fx.chroma_key", "params": {"spill_suppression": 0.2}})];
        t.migrate_ids().unwrap();
        assert_eq!(t.graph.as_ref().unwrap().ops[0]["params"], serde_json::json!({"spill": 0.2}));
        assert_eq!(t.bindings[0].property, "node:0.spill");
    }

    /// The ids every recipe and template Klipp ships names are current:
    /// reading them through the table changes nothing (#2888).
    #[test]
    fn the_shipped_files_name_current_ids() {
        let shipped: Vec<CompositionTemplate> = builtin_recipes().into_iter().chain(builtin_templates()).collect();
        assert_eq!(builtin_recipes().len(), 18);
        for t in shipped {
            let mut again = t.clone();
            again.migrate_ids().unwrap();
            assert_eq!(serde_json::to_value(&again).unwrap(), serde_json::to_value(&t).unwrap(), "{}", t.name);
        }
    }

    /// A file from someone else is bounded before its ids are read: too
    /// many ops is refused as such, not walked.
    #[test]
    fn a_recipe_past_its_limits_is_refused_before_its_ids_are_read() {
        let mut many = builtin_recipes().remove(0);
        many.graph.as_mut().unwrap().ops = vec![serde_json::json!({"op": "add", "after": {"anchor": "end"}, "type": "klipp.fx.not_here"}); MAX_RECIPE_OPS + 1];
        let why = CompositionTemplate::read(&serde_json::to_string(&many).unwrap()).unwrap_err();
        assert!(why.contains("at most"), "{why}");
    }
}
