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

/// Every recipe there is (#2757): the user's own (in `dir`, the template
/// folder), then the installed packs', then Klipp's. A name asked for
/// finds the first, so the user's own of a name comes before Klipp's.
pub fn all(dir: Option<&std::path::Path>) -> Vec<CompositionTemplate> {
    let mut recipes: Vec<CompositionTemplate> = crate::plans::template_store(dir).recipes().cloned().collect();
    recipes.extend(ct::builtin_recipes());
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
            // The title op's fields, or its text alone, made into the title
            // when the clip is known (`on_clip`).
            SlotKind::Title => match v {
                Value::String(_) | Value::Object(_) => TemplateValue::Text(v.to_string()),
                _ => return Err(format!("slot \"{}\" takes the title's text, or the title op's fields", s.label)),
            },
            // The tracked matte of the clip's media, by its model.
            SlotKind::SubjectMatte => match v.as_str().map(str::trim).map(str::to_lowercase).as_deref() {
                Some("subject") => TemplateValue::Text("Subject".into()),
                Some("foreground") => TemplateValue::Text("Foreground".into()),
                _ => return Err(format!("slot \"{}\" takes \"subject\" (one detected subject) or \"foreground\" (everything in front)", s.label)),
            },
            // One of the clip's tracked surfaces, found when the clip is
            // known (`on_clip`).
            SlotKind::Surface => TemplateValue::Text(v.as_str().ok_or_else(|| format!("slot \"{}\" takes the name or id of a surface tracked in the clip", s.label))?.trim().to_string()),
            SlotKind::Media | SlotKind::Clip => TemplateValue::Text(v.as_str().ok_or_else(|| format!("slot \"{}\" takes an id", s.label))?.to_string()),
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
    // A title slot's title, made by the title op over the clip's span.
    let mut values = values.clone();
    for (name, slot) in recipe.slots.iter().filter(|(_, s)| s.kind == SlotKind::Title) {
        let Some(TemplateValue::Text(fields)) = values.get(name) else { continue };
        let fields: Value = serde_json::from_str(fields).map_err(|e| format!("slot \"{}\": {e}", slot.label))?;
        let (start, end) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
        let title = klipp_document::ops::title_from_fields(seq, &fields, start, end - start).map_err(|e| format!("slot \"{}\": {e}", slot.label))?;
        values.insert(name.clone(), TemplateValue::Text(serde_json::to_string(&title).map_err(|e| e.to_string())?));
    }
    // A surface slot's surface, by name or id, kept by its id.
    for (name, slot) in recipe.slots.iter().filter(|(_, s)| s.kind == SlotKind::Surface) {
        let Some(TemplateValue::Text(given)) = values.get(name) else { continue };
        let found = c.surfaces.iter().find(|s| s.id.to_string() == *given).or_else(|| c.surfaces.iter().find(|s| s.name.eq_ignore_ascii_case(given)));
        let Some(surface) = found else {
            let have: Vec<String> = c.surfaces.iter().map(|s| format!("\"{}\"", s.name)).collect();
            let have = if have.is_empty() { "none yet: track_surface tracks one".to_string() } else { have.join(", ") };
            return Err(format!("slot \"{}\": the clip has no surface \"{given}\"; it has {have}", slot.label));
        };
        values.insert(name.clone(), TemplateValue::Text(surface.id.to_string()));
    }
    let values = &values;
    // Measured on this sequence and this clip: its seconds made its media's.
    let fps = f64::from(seq.fps.num) / f64::from(seq.fps.den.max(1));
    let source_at = |t: f64| c.source_secs_at(t, fps);
    let placing = ct::Placing { frame: seq.resolution, length: (c.timeline_out - c.timeline_in).to_secs_f64(), source_at: &source_at };
    let wire = wire_ops(recipe, clip, &anchors, values, &placing)?;
    let docs = crate::graph_model::documents(seq, Root::Clip { clip: clip.0 })?;
    let prepared = crate::graph_prepare::prepare(&wire, docs).map_err(|(at, why)| match at {
        Some(i) => format!("\"{}\" step {i}: {why}", recipe.name),
        None => format!("\"{}\": {why}", recipe.name),
    })?;
    // The node each op made, which its controls set later (#2758).
    let nodes = prepared.created.iter().filter_map(|(k, path)| Some((*k, path.node()?))).collect();
    let placed = RecipeInstance { name: recipe.name.clone(), instance: recipe.recipe_instance(values, &placing), nodes };
    Ok((prepared.ops, placed))
}

/// Each media file's path and rotation, by its id: where its subject
/// mattes are kept (#2758).
pub(crate) fn media_files(pool: &klipp_media::MediaPool) -> std::collections::HashMap<klipp_core::MediaId, (std::path::PathBuf, i32)> {
    pool.all().map(|m| (m.id, (m.path.clone(), m.source_rotation))).collect()
}

/// What the reply says of a recipe's subject matte (#2758): nothing when
/// the clip's media has its frames, else how many it has and that
/// `track_subject` makes the rest, only those, with the model it reads.
/// Never a model fetched: that is the person's to ask for.
pub(crate) fn subject_note(seq: &Sequence, clip: ClipId, recipe: &CompositionTemplate, values: &BTreeMap<String, TemplateValue>, media: &std::collections::HashMap<klipp_core::MediaId, (std::path::PathBuf, i32)>) -> Option<String> {
    let c = seq.all_clips().find(|c| c.id == clip)?;
    let klipp_timeline::ClipKind::Media { media_id } = &c.kind else { return None };
    let (path, rotation) = media.get(media_id)?;
    let mut said = Vec::new();
    for (name, _) in recipe.slots.iter().filter(|(_, s)| s.kind == SlotKind::SubjectMatte) {
        let Some(TemplateValue::Text(model)) = values.get(name) else { continue };
        let index = u32::from(model == "Foreground");
        let word = if index == 1 { "foreground" } else { "subject" };
        let times = klipp_compose::subject_matte::source_times(seq, c);
        let have = klipp_compose::subject_matte::media_dir(path, klipp_compose::subject_matte::decode_rotation(c, *rotation), index)
            .and_then(|dir| klipp_compose::subject_matte::coverage(&dir, &times))
            .map_or(0, |(have, _)| have);
        if have < times.len() {
            said.push(format!(
                "its {word} matte has {have} of the clip's {} frames: track_subject with model {word} makes the missing ones (it downloads the model only if you ask it to), and the text shows in front where none is made yet",
                times.len()
            ));
        }
    }
    (!said.is_empty()).then(|| said.join("; "))
}

/// The recipe's graph ops for clip `clip`, as the wire takes them.
pub(crate) fn wire_ops(recipe: &CompositionTemplate, clip: ClipId, anchors: &BTreeMap<&str, uuid::Uuid>, values: &BTreeMap<String, TemplateValue>, placed: &ct::Placing) -> Result<Vec<GraphWireOp>, String> {
    let ops = recipe.recipe_ops(clip.0, anchors, values, placed)?;
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
        assert_eq!(recipes.len(), 16, "every file in assets/recipes reads");
        for r in &recipes {
            assert_eq!(r.kind, ct::TemplateKind::Recipe, "{}", r.name);
            let v = listed(r);
            assert_eq!(v["controls"].as_array().map(Vec::len), Some(r.parameters.len()), "{}", r.name);
        }
        assert!(named(&recipes, "green SCREEN").is_ok());
        let why = named(&recipes, "no such recipe").unwrap_err();
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
        let bound = crate::plans::with_recipes(&[op("green screen", json!({"screen": "#05be05"}), json!({"Tolerance": 0.25, "Softness": 0.3}))], None, &klipp_media::MediaPool::new());
        let plan = resolve(&seq, &pool, &bound).unwrap_or_else(|r| panic!("{r:?}"));
        let [step] = plan.steps.as_slice() else { panic!("one step: {:?}", plan.steps.len()) };
        let EditCommand::PictureEdit { clip_id, edit, .. } = &step.edit else { panic!("a picture step") };
        assert_eq!(*clip_id, id);
        let mut after = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        after.restore_picture(&edit.after);
        let key = after.picture_effects().unwrap_or_default().iter().find_map(|e| match &e.effect {
            klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, soft_edge, .. } => Some((*key_color, *tolerance, *soft_edge)),
            _ => None,
        });
        // Softness is bound by its keyframe alias, `spill`: it lands on
        // the field it stands for.
        assert_eq!(key, Some(([5, 190, 5], 0.25, 0.3)), "the key, its colour, tolerance and softness as given");

        let refused = |ops: &[Op]| format!("{:?}", resolve(&seq, &pool, &crate::plans::with_recipes(ops, None, &klipp_media::MediaPool::new())).unwrap_err());
        let why = refused(&[op("no such recipe", json!({}), json!({}))]);
        assert!(why.contains("there is no recipe") && why.contains("no such recipe"), "{why}");
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
        let plan = resolve(&seq, &pool, &crate::plans::with_recipes(&ops, None, &klipp_media::MediaPool::new())).unwrap_or_else(|r| panic!("{r:?}"));
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

    /// Redact on its recipe (#2886): the box hidden in the clip's own
    /// picture, a mosaic after its display step masked to the box with a
    /// margin, keyed on from `from` to `to`, in seconds into the clip, and
    /// to nothing (a 1 px block) outside; one step, said as before. A saved recipe of the same name
    /// never stands in for Klipp's own: what a redaction hides is not
    /// someone else's file to say.
    #[test]
    fn a_redaction_is_its_recipe_in_the_clips_own_picture() {
        use klipp_document::history::EditCommand;
        use klipp_document::ops::{resolve, Anchor, ClipRef, Op};
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut seq = Sequence::new_default();
        // The clip starts 2 s in, from 5 s of its file.
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(2.0), t(6.0), t(5.0), t(9.0));
        let id = clip.id;
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        let op = Op::Redact { clip_id: ClipRef::Id(id), from: Some(Anchor::Secs(3.0)), to: Some(Anchor::Secs(5.0)), x: 0.2, y: 0.6, w: 0.3, h: 0.05, kind: "mosaic".into(), amount: None, recipes: None };
        // A recipe of the same name in the template folder, hiding nothing.
        let dir = std::env::temp_dir().join(format!("klipp-redact-own-{}", uuid::Uuid::new_v4()));
        let mut decoy = ct::builtin_recipes().into_iter().find(|r| r.name == "Glitch").unwrap();
        decoy.id = uuid::Uuid::new_v4();
        decoy.name = "Redact (mosaic)".into();
        ct::CompositionTemplateStore::load(Some(dir.clone())).save(decoy).unwrap();
        let plan = resolve(&seq, &klipp_media::MediaPool::new(), &crate::plans::with_recipes(std::slice::from_ref(&op), Some(&dir), &klipp_media::MediaPool::new())).unwrap_or_else(|r| panic!("{r:?}"));
        std::fs::remove_dir_all(&dir).ok();
        let step = &plan.steps[0];
        assert!(step.what.contains("from 3.00 to 5.00 s") && step.what.contains("in the clip's own picture"), "{}", step.what);
        let EditCommand::PictureEdit { clip_id, edit, .. } = &step.edit else { panic!("a picture step: {:?}", step.edit) };
        assert_eq!(*clip_id, id);
        let mut c = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        c.restore_picture(&edit.after);
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let tags: Vec<&str> = graph.nodes.iter().filter_map(klipp_graph_lift::effect_params::tag).collect();
        assert!(tags.contains(&"mosaic") && !tags.contains(&"vhs"), "Klipp's own recipe, not the saved one: {tags:?}");
        let mosaic = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("mosaic")).unwrap();
        let keys: Vec<(f64, f64)> = mosaic.anim.get(&klipp_graph_format::ParamKey::from("block_size")).unwrap().keys.iter().map(|k| (k["time_secs"].as_f64().unwrap(), k["value"].as_f64().unwrap())).collect();
        assert_eq!(keys.len(), 3);
        assert!((keys[0].0 - 0.999).abs() < 1e-9 && keys[0].1 == 1.0, "a 1 px mosaic, nothing, before: {keys:?}");
        assert_eq!((keys[1], keys[2]), ((1.0, 36.0), (3.0, 1.0)), "amount 0.5 through 12..60, 1 to 3 s into the clip: {keys:?}");
        let mask = graph.nodes.iter().find_map(|n| n.legacy.as_ref().and_then(|l| (l.payload["kind"] == "Mask").then(|| l.payload["params"]["masks"]["masks"][0]["shape"]["Rectangle"].clone()))).expect("the box's mask");
        let near = |k: &str, v: f64| (mask[k].as_f64().unwrap() - v).abs() < 1e-6;
        assert!(near("center_x", 0.35) && near("width", 0.312) && near("center_y", 0.625) && near("height", 0.062 * 1080.0 / 1920.0), "the box and its margin: {mask}");
        let names: Vec<&str> = graph.ui.frames.iter().filter_map(|f| f["recipe"]["name"].as_str()).collect();
        assert_eq!(names, ["Redact (mosaic)"]);
    }

    /// A behaviour's control stays live (#2758): a recipe's wiggle, put on
    /// by its modify op, takes the control's value when placed and again
    /// when template_link moves the control.
    #[test]
    fn a_behaviours_control_stays_live() {
        use klipp_document::history::EditCommand;
        use klipp_document::ops::{resolve, ClipRef, Op};
        let recipe: CompositionTemplate = serde_json::from_value(json!({
            "format_version": 2, "id": "5c2a9e71-3d4b-4f86-a1e0-7b9d2c6f4a13", "name": "Shaking", "kind": "recipe",
            "composition_id": "00000000-0000-0000-0000-000000000000",
            "parameters": { "shake": { "label": "Shake", "default": { "type": "Number", "value": 30.0 }, "range": { "min": 0.0, "max": 200.0 } } },
            "bindings": [{ "parameter": "shake", "property": "node:0.position_x~amplitude" }],
            "graph": { "ops": [
                { "op": "add", "after": { "anchor": "end" }, "type": "klipp.fx.picture_transform" },
                { "op": "modify", "path": { "new": 0 }, "key": "position_x", "modifiers": [{ "type": "wiggle", "amplitude": 0.0, "frequency": 3.0 }] }
            ] }
        }))
        .unwrap();
        let dir = std::env::temp_dir().join(format!("klipp-recipe-behaviour-{}", uuid::Uuid::new_v4()));
        ct::CompositionTemplateStore::load(Some(dir.clone())).save(recipe).unwrap();
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut seq = Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        let id = clip.id;
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        let ops = [
            Op::Recipe { clip_id: ClipRef::Id(id), name: "Shaking".into(), slots: BTreeMap::new(), controls: BTreeMap::new(), build: None },
            Op::TemplateLink { clip_id: ClipRef::Id(id), control: Some("Shake".into()), target: None, action: None, value: Some(50.0) },
        ];
        let plan = resolve(&seq, &klipp_media::MediaPool::new(), &crate::plans::with_recipes(&ops, Some(&dir), &klipp_media::MediaPool::new())).unwrap_or_else(|r| panic!("{r:?}"));
        let mut c = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        let mut amplitudes = Vec::new();
        for step in &plan.steps {
            let EditCommand::PictureEdit { edit, .. } = &step.edit else { panic!("a picture step: {:?}", step.edit) };
            c.restore_picture(&edit.after);
            let graph = c.picture_v2().unwrap().picture.graph.clone();
            let mv = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("picture_transform")).unwrap();
            let anim = mv.anim.get(&klipp_graph_format::ParamKey::from("position_x")).expect("the wiggle");
            amplitudes.push(anim.modifiers.as_ref().unwrap()["modifiers"][0]["amplitude"].as_f64().unwrap());
        }
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(amplitudes, [30.0, 50.0], "placed at the control's default, then where it was moved");
    }

    /// A recipe's saved node entry (#2757 step 6) is checked as the node's
    /// own before it is used: one of another type, and one that doesn't
    /// read as that node's, are refused, the clip left as it was.
    #[test]
    fn a_hostile_saved_entry_is_refused() {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut seq = Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        let id = clip.id;
        seq.tracks[0].clips = vec![clip];
        for entry in [json!({ "type": "chroma_key" }), json!({ "type": "glow", "radius": "huge" }), json!("not an entry")] {
            let recipe: CompositionTemplate = serde_json::from_value(json!({
                "format_version": 2, "id": "1f2e3d4c-5b6a-4978-8a9b-0c1d2e3f4a5b", "name": "Hostile", "kind": "recipe",
                "composition_id": "00000000-0000-0000-0000-000000000000",
                "graph": { "ops": [{ "op": "add", "after": { "anchor": "end" }, "type": "klipp.fx.glow", "entry": entry }] }
            }))
            .unwrap();
            assert!(on_clip(&seq, id, &recipe, &BTreeMap::new()).is_err(), "{entry} refused");
        }
    }

    /// Text behind a subject (#2758) whose clip's media has no subject
    /// matte yet: the recipe is placed, and the reply says how many of the
    /// clip's frames have one and that track_subject makes the rest, by
    /// the model the slot names; nothing is fetched. With every frame there,
    /// nothing is said. A slot given another model is refused, said.
    #[test]
    fn a_missing_subject_matte_is_said_with_how_to_make_it() {
        let recipes = ct::builtin_recipes();
        let r = named(&recipes, "Text behind subject").unwrap();
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(24, 1);
        let media = klipp_core::MediaId::new();
        let clip = klipp_timeline::Clip::media(media, t(0.0), t(1.0), t(0.0), t(1.0));
        let id = clip.id;
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        let file = std::env::temp_dir().join(format!("klipp-no-matte-{}.mov", uuid::Uuid::new_v4()));
        let files = std::collections::HashMap::from([(media, (file, 0))]);
        let slots = BTreeMap::from([("title".to_string(), json!("Hello")), ("Subject".to_string(), json!("foreground"))]);
        let v = values(r, &slots, &BTreeMap::new()).unwrap();
        let note = subject_note(&seq, id, r, &v, &files).expect("said");
        assert!(note.contains("has 0 of the clip's") && note.contains("track_subject with model foreground") && note.contains("only if you ask"), "{note}");
        assert!(values(r, &BTreeMap::from([("subject".to_string(), json!("cat"))]), &BTreeMap::new()).unwrap_err().contains("\"subject\""));
        // The title slot is the title op's: its text made into a title.
        let (ops, _) = on_clip(&seq, id, r, &v).unwrap();
        assert!(!ops.is_empty());
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
