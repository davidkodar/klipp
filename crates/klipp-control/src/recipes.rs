//! Recipes (#2758): node groups with slots and controls, saved in the
//! template file format and built into a clip's picture as one ordinary
//! graph batch. One op places any of them and one call lists them; a
//! recipe is a file, never code of its own.

use std::collections::BTreeMap;

use klipp_core::{ClipId, RecipeInstance, TemplateValue};
use klipp_graph_format::path::Root;
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::composition_template::{self as ct, CompositionTemplate, SlotKind};
use klipp_timeline::Sequence;
use serde_json::{json, Value};

use crate::graph_wire::GraphWireOp;

/// Every recipe there is: Klipp's own, then those saved in `dir` (the
/// template folder). A saved one named like a built-in is listed after it.
pub fn all(dir: Option<std::path::PathBuf>) -> Vec<CompositionTemplate> {
    let mut recipes = ct::builtin_recipes();
    recipes.extend(ct::CompositionTemplateStore::load(dir).recipes().cloned());
    recipes
}

/// The recipe called `name` (ignoring case), among `recipes`.
pub fn named<'a>(recipes: &'a [CompositionTemplate], name: &str) -> Result<&'a CompositionTemplate, String> {
    recipes.iter().find(|r| r.name.eq_ignore_ascii_case(name.trim())).ok_or_else(|| {
        format!("no recipe called \"{}\"; there are {}", name.trim(), recipes.iter().map(|r| format!("\"{}\"", r.name)).collect::<Vec<_>>().join(", "))
    })
}

/// A recipe as the list call gives it: its slots and its controls.
pub fn listed(r: &CompositionTemplate) -> Value {
    let controls: Vec<Value> = r.parameters.iter().map(|(name, p)| {
        let range = p.range.as_ref().map(|r| json!([r.min, r.max]));
        json!({ "name": name, "label": p.label, "default": p.default, "range": range })
    }).collect();
    let slots: Vec<Value> = r.slots.iter().map(|(name, s)| json!({ "name": name, "label": s.label, "kind": s.kind })).collect();
    json!({ "name": r.name, "category": r.category, "slots": slots, "controls": controls })
}

/// An agent's values for `recipe`'s slots and controls, by name or label:
/// a control's number, flag or text; a media slot's media id; a colour
/// slot's `#rrggbb` or `[r, g, b]` (0 to 1).
pub fn values(recipe: &CompositionTemplate, slots: &BTreeMap<String, Value>, controls: &BTreeMap<String, Value>) -> Result<BTreeMap<String, TemplateValue>, String> {
    let mut out = BTreeMap::new();
    for (given, v) in controls {
        let (name, p) = recipe.parameters.iter().find(|(n, p)| n.eq_ignore_ascii_case(given) || p.label.eq_ignore_ascii_case(given))
            .ok_or_else(|| format!("\"{}\" has no control \"{given}\"; it has {}", recipe.name, recipe.parameters.values().map(|p| format!("\"{}\"", p.label)).collect::<Vec<_>>().join(", ")))?;
        let value = match (&p.default, v) {
            (TemplateValue::Number(_), Value::Number(n)) => TemplateValue::Number(n.as_f64().unwrap_or_default()),
            (TemplateValue::Bool(_), Value::Bool(b)) => TemplateValue::Bool(*b),
            (TemplateValue::Text(_), Value::String(s)) => TemplateValue::Text(s.clone()),
            (TemplateValue::Enum(_), Value::String(s)) => TemplateValue::Enum(s.clone()),
            (TemplateValue::Color(_), v) => TemplateValue::Color(colour(v)?),
            _ => return Err(format!("control \"{}\" takes a value like {}", p.label, serde_json::to_string(&p.default).unwrap_or_default())),
        };
        out.insert(name.clone(), value);
    }
    for (given, v) in slots {
        let (name, s) = recipe.slots.iter().find(|(n, s)| n.eq_ignore_ascii_case(given) || s.label.eq_ignore_ascii_case(given))
            .ok_or_else(|| format!("\"{}\" has no slot \"{given}\"", recipe.name))?;
        let value = match s.kind {
            SlotKind::Colour => TemplateValue::Color(colour(v)?),
            SlotKind::Media | SlotKind::Clip | SlotKind::Title | SlotKind::SubjectMatte => {
                TemplateValue::Text(v.as_str().ok_or_else(|| format!("slot \"{}\" takes an id", s.label))?.to_string())
            }
        };
        out.insert(name.clone(), value);
    }
    Ok(out)
}

/// `#rrggbb`, or `[r, g, b]` / `[r, g, b, a]` from 0 to 1.
fn colour(v: &Value) -> Result<[f32; 4], String> {
    if let Some(hex) = v.as_str().and_then(|s| s.strip_prefix('#')).filter(|h| h.len() == 6) {
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map(|b| b as f32 / 255.0).map_err(|_| format!("{v} is not a colour"));
        return Ok([byte(0)?, byte(2)?, byte(4)?, 1.0]);
    }
    let parts: Vec<f32> = v.as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()).unwrap_or_default();
    match parts.as_slice() {
        [r, g, b] => Ok([*r, *g, *b, 1.0]),
        [r, g, b, a] => Ok([*r, *g, *b, *a]),
        _ => Err(format!("{v} is not a colour; give \"#rrggbb\" or [r, g, b] from 0 to 1")),
    }
}

/// `recipe` with `values` as the graph batch that builds it into the
/// picture of clip `clip` of `seq`, prepared as the graph op prepares
/// one: the ops one undo step applies. The same route places it in the
/// app and in the recipe harness.
pub fn on_clip(seq: &Sequence, clip: ClipId, recipe: &CompositionTemplate, values: &BTreeMap<String, TemplateValue>) -> Result<(Vec<GraphOp>, RecipeInstance), String> {
    let c = seq.all_clips().find(|c| c.id == clip).ok_or("no such clip on the open sequence")?;
    let anchors = ct::recipe_anchors(c)?;
    let wire = wire_ops(recipe, clip, &anchors, values, seq.resolution)?;
    let docs = crate::graph_model::documents(seq, Root::Clip { clip: clip.0 })?;
    let prepared = crate::graph_prepare::prepare(&wire, docs).map_err(|(at, why)| match at {
        Some(i) => format!("\"{}\" step {i}: {why}", recipe.name),
        None => format!("\"{}\": {why}", recipe.name),
    })?;
    // The node each op made, which its controls set later (#2758).
    let nodes = prepared.created.iter().filter_map(|(k, path)| Some((*k, path.node()?))).collect();
    let placed = RecipeInstance { name: recipe.name.clone(), instance: recipe.recipe_instance(values, seq.resolution), nodes };
    Ok((prepared.ops, placed))
}

/// The recipe's graph ops for clip `clip`, as the wire takes them.
pub(crate) fn wire_ops(recipe: &CompositionTemplate, clip: ClipId, anchors: &BTreeMap<&str, uuid::Uuid>, values: &BTreeMap<String, TemplateValue>, frame: klipp_core::Resolution) -> Result<Vec<GraphWireOp>, String> {
    let ops = recipe.recipe_ops(clip.0, anchors, values, frame)?;
    ops.into_iter().enumerate().map(|(i, op)| serde_json::from_value(op).map_err(|e| format!("\"{}\" step {i}: {e}", recipe.name))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in recipe reads, is a recipe, and lists its slots and
    /// controls by name and label.
    #[test]
    fn every_built_in_recipe_reads_and_lists() {
        let recipes = ct::builtin_recipes();
        assert_eq!(recipes.len(), 3, "every file in assets/recipes reads");
        for r in &recipes {
            assert_eq!(r.kind, ct::TemplateKind::Recipe, "{}", r.name);
            let v = listed(r);
            assert_eq!(v["controls"].as_array().map(Vec::len), Some(r.parameters.len()), "{}", r.name);
        }
        assert!(named(&recipes, "green SCREEN").is_ok());
        let why = named(&recipes, "sky").unwrap_err();
        assert!(why.contains("\"Green screen\""), "names those there are: {why}");
    }

    /// The recipe op, bound as the socket binds it, builds the recipe into
    /// the clip as one picture step: a key node after the chain's end,
    /// its colour, tolerance and softness set. A name no recipe has and a slot left
    /// unfilled are refused, said.
    #[test]
    fn the_recipe_op_is_one_picture_step() {
        use klipp_document::history::EditCommand;
        use klipp_document::ops::{resolve, ClipRef, Op};
        let mut seq = Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), klipp_core::RationalTime::from_secs_f64(0.0, 25), klipp_core::RationalTime::from_secs_f64(4.0, 25), klipp_core::RationalTime::from_secs_f64(0.0, 25), klipp_core::RationalTime::from_secs_f64(4.0, 25));
        let id = clip.id;
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        let pool = klipp_media::MediaPool::new();
        let op = |name: &str, slots: Value, controls: Value| Op::Recipe {
            clip_id: ClipRef::Id(id), name: name.into(),
            slots: serde_json::from_value(slots).unwrap(), controls: serde_json::from_value(controls).unwrap(), build: None,
        };
        let bound = crate::plans::with_recipes(&[op("green screen", json!({"screen": "#05be05"}), json!({"Tolerance": 0.25, "Softness": 0.3}))], None);
        let plan = resolve(&seq, &pool, &bound).unwrap_or_else(|r| panic!("{r:?}"));
        let [step] = plan.steps.as_slice() else { panic!("one step: {:?}", plan.steps.len()) };
        let EditCommand::PictureEdit { clip_id, edit, .. } = &step.edit else { panic!("a picture step") };
        assert_eq!(*clip_id, id);
        let mut after = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        after.restore_picture(&edit.after);
        let key = after.builtin_effects().iter().find_map(|e| match e {
            klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, soft_edge, .. } => Some((*key_color, *tolerance, *soft_edge)),
            _ => None,
        });
        // Softness is bound by its keyframe alias, `spill`: it lands on
        // the field it stands for.
        assert_eq!(key, Some(([5, 190, 5], 0.25, 0.3)), "the key, its colour, tolerance and softness as given");

        let refused = |ops: &[Op]| format!("{:?}", resolve(&seq, &pool, &crate::plans::with_recipes(ops, None)).unwrap_err());
        let why = refused(&[op("sky", json!({}), json!({}))]);
        assert!(why.contains("there is no recipe") && why.contains("sky"), "{why}");
        assert!(refused(&[op("Green screen", json!({}), json!({}))]).contains("fill the slot 'screen'"));
    }

    /// A placed recipe's controls stay live (#2758): its frame holds the
    /// nodes it made and its instance, and template_link moves a control
    /// in one picture step, every number it drives following (Size sets
    /// both scales), a plain number control (Tolerance)
    /// too; a target set by hand keeps its offset when the control moves.
    #[test]
    fn a_placed_recipes_controls_stay_live() {
        use klipp_document::history::EditCommand;
        use klipp_document::ops::{resolve, ClipRef, Op};
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: 1000, height: 500 };
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        let id = clip.id;
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        let pool = klipp_media::MediaPool::new();
        let link = |control: Option<&str>, target: Option<&str>, value: f64| Op::TemplateLink { clip_id: ClipRef::Id(id), control: control.map(Into::into), target: target.map(Into::into), action: None, value: Some(value) };
        let place = |name: &str, slots: Value| Op::Recipe { clip_id: ClipRef::Id(id), name: name.into(), slots: serde_json::from_value(slots).unwrap(), controls: BTreeMap::new(), build: None };
        let ops = [
            place("Picture in picture", json!({"picture": "m1"})),
            place("Green screen", json!({"screen": "#05be05"})),
            link(Some("Size"), None, 0.5),
            link(Some("tolerance"), None, 0.4),
            link(None, Some("node:3.scale_y"), 0.9),
            link(Some("Size"), None, 1.0),
        ];
        let plan = resolve(&seq, &pool, &crate::plans::with_recipes(&ops, None)).unwrap_or_else(|r| panic!("{r:?}"));
        assert_eq!(plan.steps.len(), 6, "each one step");
        let mut c = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        let mut at = Vec::new();
        for step in &plan.steps {
            let EditCommand::PictureEdit { edit, .. } = &step.edit else { panic!("a picture step: {:?}", step.edit) };
            c.restore_picture(&edit.after);
            // Read off the picture's nodes: the branch's Transform is
            // in no effect list.
            let graph = c.picture_v2().unwrap().picture.graph.clone();
            let entry = |tag: &str| graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some(tag)).and_then(|n| klipp_graph_lift::effect_params::entry_of(n).ok());
            let f = |v: &Value, k: &str| v[k].as_f64().unwrap() as f32;
            let scale = entry("transform").map(|e| (f(&e, "scale_x"), f(&e, "scale_y")));
            let tol = entry("chroma_key").map(|e| f(&e, "tolerance"));
            at.push((scale, tol));
        }
        let close = |a: Option<(f32, f32)>, b: (f32, f32)| a.is_some_and(|a| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4);
        assert!(close(at[0].0, (0.35, 0.35)), "placed at its default: {:?}", at[0]);
        assert!(close(at[2].0, (0.5, 0.5)), "Size 0.5 moves both scales: {:?}", at[2]);
        assert_eq!(at[3].1, Some(0.4), "a plain number control is live too");
        assert!(close(at[4].0, (0.5, 0.9)), "scale_y set by hand: {:?}", at[4]);
        // Size at 1: x follows; y keeps its hand offset (+0.4).
        assert!(close(at[5].0, (1.0, 1.4)), "the hand offset kept: {:?}", at[5]);
        let frames = &c.picture_v2().unwrap().picture.graph.ui.frames;
        let names: Vec<&str> = frames.iter().filter_map(|f| f["recipe"]["name"].as_str()).collect();
        assert_eq!(names, ["Picture in picture", "Green screen"], "a frame for each, holding its controls");
    }

    /// Values are taken by name or label, colours as hex or a list, and
    /// a name the recipe lacks is refused, said.
    #[test]
    fn values_by_name_or_label() {
        let recipes = ct::builtin_recipes();
        let r = named(&recipes, "Green screen").unwrap();
        let slots = BTreeMap::from([("Screen colour".to_string(), json!("#00ff00"))]);
        let controls = BTreeMap::from([("Tolerance".to_string(), json!(0.4)), ("spill".to_string(), json!(0.2))]);
        let v = values(r, &slots, &controls).unwrap();
        assert_eq!(v["screen"], TemplateValue::Color([0.0, 1.0, 0.0, 1.0]));
        assert_eq!(v["tolerance"], TemplateValue::Number(0.4));
        assert_eq!(v["spill"], TemplateValue::Number(0.2));
        let why = values(r, &BTreeMap::new(), &BTreeMap::from([("glow".to_string(), json!(1.0))])).unwrap_err();
        assert!(why.contains("\"Tolerance\""), "{why}");
    }
}
