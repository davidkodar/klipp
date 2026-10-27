//! M3: an effect card's own switch and its Mix, on the effects stack,
//! held to what they must store.
//! - **The eye** (`@enabled`): the card's checkbox switched off and on,
//!   the picture exactly the clip's with that effect's enabled flag so
//!   set, one step, undone and redone exactly; the frame changes, and the
//!   CPU's is the GPU's.
//! - **Mix** (`@mix`): held against the path-op oracle (`SetMixAt` applied
//!   by path): a drag, a typed value, a reset from its label, Escape on a
//!   drag, undo and the frame. Its keys are counted under the variant "new
//!   only" until the variants are renamed in one sweep. A type that takes
//!   no mix (Invert) draws none, and a mix sent to it by path is refused.
//! - **A hidden field** (a seed, an analysis' gain, a focus band): the card
//!   draws no row for it, and another row's edit leaves it exactly as it
//!   was (`PreserveHidden`).
//! - **An effect's workflow** with nothing of its own to drive there (its
//!   rows carry the edits): declared on the stack.

use super::parity_controls::{fixture, Stack};
use super::parity_manifest::{catalog, CatalogEntry, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface};
use klipp_core::AppCommand;
use klipp_effects::registry::{effect_types, EffectType};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip;

/// The variant Mix's keys are counted under (renamed with the others in
/// one sweep).
const NEW_ONLY: &str = "new only";

/// The stack's cards: an entry of `parameter` for every effect type whose
/// card the stack draws, with the type.
fn entries(parameter: &str) -> Vec<(CatalogEntry, &'static EffectType)> {
    catalog()
        .into_iter()
        .filter(|e| !e.grade && e.parameter == parameter)
        .filter_map(|e| {
            let ty = effect_types().find(|t| t.desc.id == e.type_id)?;
            Some((e, ty))
        })
        .collect()
}

/// The effects' own workflows whose rows carry their edits, with nothing
/// of their own to drive on the card (an analysis' run is the specialists'
/// family's).
const PLAIN_WORKFLOWS: &[&str] = &["temporal_denoise", "speed_ramp", "focus_blur", "color_fade", "corner_pin"];

fn workflows() -> Vec<CatalogEntry> {
    catalog().into_iter().filter(|e| !e.grade && matches!(e.value, CatalogValue::Workflow(id) if PLAIN_WORKFLOWS.contains(&id))).collect()
}

/// The effect types' hidden fields: a field no card draws a row for.
fn hidden() -> Vec<(CatalogEntry, &'static EffectType, &'static klipp_effects::registry::ParamDesc)> {
    catalog()
        .into_iter()
        .filter(|e| !e.grade)
        .filter_map(|e| {
            let CatalogValue::Parameter { desc, presentation } = e.value else { return None };
            (presentation.widget == klipp_effects::registry::presentation::Widget::Hidden).then_some(())?;
            let ty = effect_types().find(|t| t.desc.id == e.type_id)?;
            Some((e, ty, desc))
        })
        .collect()
}

/// The cards the stack draws an eye on: every effect's, a placed kind's
/// included (its card's commands go through `to_effect_list`).
fn eyes() -> Vec<(CatalogEntry, &'static EffectType)> {
    entries("@enabled")
}

/// Switching a Speed Ramp off can't change the frame: retiming comes from
/// the clip's speed curve, and the effect keeps only its frame blending.
const RAMP_PIXELS: &str = "known defect #1409: retiming comes from the clip's speed curve, so a Speed Ramp switched off draws the same frame";

/// A Title Source draws its title as the foreground a Merge beside the
/// chain lays over; in the stack's list alone it is a clear picture.
const TITLE_SOURCE_PIXELS: &str = "draws only through a Merge beside the chain (#2758), drawn on both routes in klipp-compose's title_source_render; alone in the list it is a clear picture";

/// Why a card's eye earns no Pixels key, when it can't: switching it can't
/// change the frame, or the frame differs between the CPU and the GPU
/// stack before any edit (pinned with its issue until the routes agree).
fn eye_pixels_pinned(tag: &str) -> Option<&'static str> {
    match tag {
        "speed_ramp" => Some(RAMP_PIXELS),
        "title_source" => Some(TITLE_SOURCE_PIXELS),
        _ => None,
    }
}

pub(super) fn require(manifest: &mut Manifest) {
    for (entry, _, _) in hidden() {
        manifest.require_fixture(&entry, Surface::EffectsStack, "sample");
    }
    for entry in workflows() {
        manifest.require_fixture(&entry, Surface::EffectsStack, "sample");
    }
    for (entry, ty) in eyes() {
        manifest.require_fixture(&entry, Surface::EffectsStack, "sample");
        if let Some(why) = eye_pixels_pinned(ty.tag) {
            manifest.exclude_key(CoverageKey::new(entry.type_id, &entry.parameter, Surface::EffectsStack, "sample", Interaction::Pixels), why);
        }
    }
    for (entry, _) in entries("@mix") {
        manifest.require_fixture(&entry, Surface::EffectsStack, NEW_ONLY);
    }
}

/// `clip` with its effect at `index` switched `on`, every effect keeping
/// its id (a plain `set_effect_list` gives each a new one).
fn switched(mut clip: klipp_timeline::Clip, index: usize, on: bool) -> klipp_timeline::Clip {
    let effects = clip.builtin_effects().to_vec();
    let mut enabled = clip.builtin_effects_enabled().to_vec();
    enabled.resize(effects.len(), true);
    enabled[index] = on;
    let ids = clip.effect_id_list();
    clip.set_effect_list_with_ids(effects, enabled, &ids);
    clip
}

fn is_on(s: &Stack, index: usize) -> bool {
    test_clip::enabled(s.clip()).get(index).copied().unwrap_or(true)
}

/// Click the card's eye for the effect at `index`: the first point left of
/// a drawn text on its line whose click switches that effect, and nothing
/// else, found on a throwaway stack over the same clip.
fn click_eye(s: &mut Stack, index: usize) -> Result<(), String> {
    let was = is_on(s, index);
    let drawn = s.frame(Vec::new());
    for (_, at) in &drawn {
        for x in (2..40).map(|i| 2.0 * i as f32).filter(|x| *x < at.x) {
            let p = egui::pos2(x, at.y);
            let mut t = Stack::new(s.clip().clone());
            t.state.selected_clip = s.state.selected_clip;
            t.frame(Vec::new());
            t.frame(vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
            t.frame(vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
            if is_on(&t, index) != was && t.unapplied.is_empty() && test_clip::effects(t.clip()) == test_clip::effects(s.clip()) {
                s.frame(vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
                s.frame(vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
                s.frame(Vec::new());
                return Ok(());
            }
        }
    }
    Err("no eye on the card".into())
}

/// The clip whose first effect's card the eye cases switch: `effect`
/// alone, or, for an Opacity, with a second after it, which is the clip's
/// own opacity (the anchor, drawn in the Video tab), so the first is a card.
fn eye_fixture(effect: &BuiltinEffect) -> klipp_timeline::Clip {
    let mut clip = fixture(effect);
    if matches!(effect, BuiltinEffect::Opacity { .. }) {
        clip.push_effect(BuiltinEffect::Opacity { value: 1.0 });
    }
    clip
}

/// The eye switched `on` from the other way: the picture exactly the
/// start's with the effect's enabled flag set `on`, one step, undone and
/// redone exactly.
fn eye_case(effect: &BuiltinEffect, on: bool) -> Result<(), String> {
    let start = switched(eye_fixture(effect), 0, !on);
    let expected = Stack::new(switched(start.clone(), 0, on)).picture();
    let mut new = Stack::new(start);
    let before = new.picture();
    click_eye(&mut new, 0)?;
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", new.unapplied));
    }
    if is_on(&new, 0) != on {
        return Err(format!("switched {}, want {on}", is_on(&new, 0)));
    }
    let after = new.picture();
    if after != expected {
        return Err("not the expected picture: more than the effect's switch was written".into());
    }
    let steps = new.state.active_history().undo_count();
    if steps != 1 {
        return Err(format!("{steps} undo steps, not one"));
    }
    new.step(false)?;
    if new.picture() != before {
        return Err("undo doesn't give back the picture before".into());
    }
    new.step(true)?;
    if new.picture() != after {
        return Err("redo doesn't give back the picture after".into());
    }
    Ok(())
}

/// The routes an effect's frame is drawn on: the CPU alone for a type that
/// runs there by design (`parity_numeric::CPU_BY_DESIGN`), both otherwise.
fn routes(ty: &EffectType) -> (super::parity_pixels::Routes, (u32, u32)) {
    use super::parity_pixels::{Routes, FULL, SMALL};
    let routes = if super::parity_numeric::CPU_BY_DESIGN.iter().any(|(t, _)| *t == ty.desc.id) { Routes::CpuByDesign } else { Routes::Both };
    (routes, if super::parity_numeric::AT_FULL_SIZE.contains(&ty.tag) { FULL } else { SMALL })
}

/// The eye switched off, drawn: on the effect's pixel fixture, the card's
/// eye changes the frame, and the CPU's frame is the GPU's.
fn eye_pixels(ty: &EffectType) -> Result<(), String> {
    // The frei0r card's sample names no plug-in, and one that isn't there
    // passes its frame on: a real one, set to change the frame. (The old
    // chain round-tripped the frame through 8 bits for any frei0r entry,
    // which made an empty one look as if it acted.)
    let sample = match ty.tag {
        "frei0r" => {
            let info = super::parity_specialist_pixels::saturator()?;
            let params = info.params.iter().enumerate().map(|(i, p)| if i == 0 { klipp_frei0r::Frei0rValue::Double(0.85) } else { klipp_frei0r::Frei0rValue::default_for(p.kind) }).collect();
            BuiltinEffect::Frei0r { type_id: info.type_id(), params }
        }
        _ => super::parity_numeric::acting_sample(ty),
    };
    let (mut start, index) = super::parity_numeric::pixel_fixture(ty, &sample, Surface::EffectsStack)?;
    if ty.tag == "opacity" {
        start.push_effect(BuiltinEffect::Opacity { value: 1.0 });
    }
    let (routes, size) = routes(ty);
    let start = acting(ty, start, index, size)?;
    let mut new = Stack::new(start.clone());
    click_eye(&mut new, index)?;
    if is_on(&new, index) {
        return Err("the eye didn't switch the effect off".into());
    }
    let b = new.clip().clone();
    super::parity_pixels::clip_edit_drawn_on(routes, size, &start, &b)?;
    // A Transform switched off is bypassed (#1207): its frame is the clip's
    // with no Transform at all, exactly, on each route.
    if ty.tag == "transform" {
        let mut without = start.clone();
        let (effects, enabled): (Vec<_>, Vec<_>) = without.builtin_effects().iter().cloned().zip(without.builtin_effects_enabled().iter().copied().chain(std::iter::repeat(true))).enumerate().filter(|(i, _)| *i != index).map(|(_, p)| p).unzip();
        without.set_effect_list(effects, enabled);
        // At the oracle's time, as `same_edit_on` draws.
        let draw = |c: &klipp_timeline::Clip| super::parity_pixels::frames_at(super::parity_pixels::Routes::Both, size, c, super::parity_pixels::AT);
        let (off, bare) = (draw(&b)?, draw(&without)?);
        if off.0.iter().zip(&bare.0).any(|(x, y)| x.to_bits() != y.to_bits()) || off.0.len() != bare.0.len() {
            return Err("Transform switched off isn't the untransformed CPU frame (#1207)".into());
        }
        if off.1 != bare.1 {
            return Err("Transform switched off isn't the untransformed GPU frame (#1207)".into());
        }
    }
    Ok(())
}

/// The path of the effect at `index` of `s`'s clip, as the card names it.
fn effect_path(clip: &klipp_timeline::Clip, ty: &EffectType) -> Result<klipp_graph_format::path::NodePath, String> {
    let picture = clip.picture_v2().map_err(|e| e.to_string())?;
    let node = picture.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some(ty.tag)).map(|n| n.id).ok_or("no node for the effect")?;
    Ok(klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip.id.0 }, [node]))
}

/// The effect's stored mix (`None`: full).
fn mix_of(s: &Stack, ty: &EffectType) -> Option<f32> {
    let picture = s.clip().picture_v2().ok()?;
    picture.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some(ty.tag)).and_then(|n| n.mix)
}

/// The Mix row's number, and its label.
fn mix_row(s: &mut Stack) -> Result<(egui::Pos2, egui::Pos2), String> {
    let drawn = s.frame(Vec::new());
    let label = drawn.iter().rev().find(|(t, _)| t == "Mix").map(|(_, p)| *p).ok_or("no Mix on the card")?;
    let number = drawn.iter().filter(|(t, p)| (p.y - label.y).abs() < 6.0 && p.x > label.x && t.parse::<f32>().is_ok()).map(|(_, p)| *p).min_by(|a, b| a.x.total_cmp(&b.x)).ok_or("no number on the Mix row")?;
    Ok((number, label))
}

fn press(p: egui::Pos2, down: bool) -> egui::Event {
    egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: down, modifiers: egui::Modifiers::NONE }
}

/// The Mix row's label dragged left (from full), held, or (`escape`)
/// taken back with Escape before the release.
fn drag_mix(s: &mut Stack, escape: bool) -> Result<(), String> {
    let (_, at) = mix_row(s)?;
    s.frame(vec![egui::Event::PointerMoved(at), press(at, true)]);
    for dx in [-8.0, -16.0, -24.0, -32.0] {
        s.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
    }
    if escape {
        s.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    }
    s.frame(vec![press(at + egui::vec2(-32.0, 0.0), false)]);
    s.frame(vec![egui::Event::PointerGone]);
    Ok(())
}

/// A value typed into the Mix slider's number.
fn type_mix(s: &mut Stack, text: &str) -> Result<(), String> {
    let (number, _) = mix_row(s)?;
    s.frame(vec![egui::Event::PointerMoved(number), press(number, true)]);
    s.frame(vec![egui::Event::PointerMoved(number), press(number, false)]);
    s.frame(vec![egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }]);
    s.frame(vec![egui::Event::Text(text.into())]);
    s.frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    s.frame(Vec::new());
    Ok(())
}

/// One Mix edit (`drive`) on the generated card against the path-op
/// oracle: the mix it stored set by path on a twin gives the same picture;
/// one step, undone and redone exactly.
fn mix_case(effect: &BuiltinEffect, ty: &EffectType, drive: impl FnOnce(&mut Stack) -> Result<(), String>) -> Result<Option<f32>, String> {
    let clip = fixture(effect);
    let mut new = Stack::new(clip.clone());
    let before = new.picture();
    drive(&mut new)?;
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", new.unapplied));
    }
    let mix = mix_of(&new, ty);
    let after = new.picture();
    if after == before {
        return Err("the Mix changed nothing".into());
    }
    let mut oracle = Stack::new(clip.clone());
    oracle.apply(&AppCommand::SetMixAt { addr: effect_path(&clip, ty)?, mix });
    if !oracle.unapplied.is_empty() || oracle.picture() != after {
        return Err(format!("not the path op's picture (mix {mix:?}): {:?}", oracle.unapplied));
    }
    if new.state.active_history().undo_count() != 1 {
        return Err(format!("{} steps, not one", new.state.active_history().undo_count()));
    }
    new.step(false)?;
    if new.picture() != before {
        return Err("undo doesn't give back the picture before".into());
    }
    new.step(true)?;
    if new.picture() != after {
        return Err("redo doesn't give back the picture after".into());
    }
    Ok(mix)
}

/// A hidden field kept: `desc` set apart from its sample value, the card
/// opened (writing nothing), and another of the effect's number rows set by
/// path as the card sends it; the field is exactly as it was set.
fn hidden_case(ty: &EffectType, desc: &klipp_effects::registry::ParamDesc) -> Result<(), String> {
    let mut json = serde_json::to_value((ty.sample)()).map_err(|e| e.to_string())?;
    // A field the stored form never carries (engine-transient, skipped in
    // serialisation): kept by never being written.
    let transient = json.get(desc.field).is_none();
    if !transient {
        json[desc.field] = match &json[desc.field] {
            serde_json::Value::Bool(b) => serde_json::Value::Bool(!b),
            serde_json::Value::Number(n) if n.is_u64() || n.is_i64() => serde_json::json!(n.as_i64().unwrap_or(0) + 7),
            serde_json::Value::Number(n) => serde_json::json!(n.as_f64().unwrap_or(0.0) * 0.5 + 0.13),
            // An unset reference (the Transform's focal anchor): a glyph's.
            serde_json::Value::Null if desc.field == "focal_ref" => serde_json::json!({ "kind": "glyph", "title_clip_id": uuid::Uuid::from_u128(7), "char_index": 3, "fallback_xy": [0.4, 0.6] }),
            // A name or a stored text (a surface, a title): another.
            serde_json::Value::String(t) => serde_json::Value::String(format!("{t}kept")),
            // A colour or a list of numbers (a reflection's water): each
            // moved by 7, inside a byte.
            serde_json::Value::Array(a) if a.iter().all(|v| v.is_u64()) => a.iter().map(|v| serde_json::json!((v.as_u64().unwrap_or(0) + 7) % 256)).collect(),
            other => return Err(format!("{}: no value apart from {other} to set", desc.field)),
        };
    }
    let effect: BuiltinEffect = serde_json::from_value(json).map_err(|e| format!("{}: {e}", desc.field))?;
    let clip = fixture(&effect);
    // As stored: the value read back, so an f32 compares as stored.
    let field = |s: &Stack| serde_json::to_value(&test_clip::effects(s.clip())[0]).ok().and_then(|v| v.get(desc.field).cloned());
    let mut s = Stack::new(clip.clone());
    let kept = field(&s);
    let before = s.picture();
    s.frame(Vec::new());
    s.frame(Vec::new());
    if !s.unapplied.is_empty() || s.picture() != before || s.state.active_history().undo_count() != 0 {
        return Err("opening the card wrote".into());
    }
    // Another row of the card edited, as it sends it: a number row, or,
    // for a type with none of its own, its Mix.
    let other = ty
        .desc
        .params
        .iter()
        .find(|p| p.key != desc.key && matches!(ty.presentation(p).widget, klipp_effects::registry::presentation::Widget::Scrub | klipp_effects::registry::presentation::Widget::Slider | klipp_effects::registry::presentation::Widget::Gradient(_)));
    let addr = effect_path(&clip, ty)?;
    let edited = match other {
        // A placed kind's card sends its edit through the clip's effect
        // list (`to_effect_list`): the effect with the field changed.
        Some(other) if crate::inspector::stack_view::is_placed_kind(ty.tag) => {
            let (lo, hi) = other.soft.or(other.range).unwrap_or((0.0, 1.0));
            let mut e = serde_json::to_value(&test_clip::effects(s.clip())[0]).map_err(|e| e.to_string())?;
            e[other.field] = serde_json::json!(lo + (hi - lo) * 0.37);
            let effect: BuiltinEffect = serde_json::from_value(e).map_err(|e| e.to_string())?;
            s.apply(&AppCommand::UpdateBuiltinEffect { clip_id: s.clip, index: 0, effect });
            other.key
        }
        Some(other) => {
            let (lo, hi) = other.soft.or(other.range).unwrap_or((0.0, 1.0));
            s.apply(&AppCommand::SetParamAt { addr, key: other.key.into(), value: klipp_graph_format::ParamValue::Float(lo + (hi - lo) * 0.37) });
            other.key
        }
        None if ty.desc.mixable => {
            s.apply(&AppCommand::SetMixAt { addr, mix: Some(0.5) });
            "its mix"
        }
        // Nothing else on the card to edit (a Title Source holds only its
        // title): opening it, which wrote nothing, is the case.
        None => return if field(&s) == kept { Ok(()) } else { Err(format!("{} changed on opening", desc.field)) },
    };
    if !s.unapplied.is_empty() {
        return Err(format!("editing {edited}: unapplied {:?}", s.unapplied));
    }
    if s.picture() == before {
        return Err(format!("editing {edited} changed nothing"));
    }
    if field(&s) != kept {
        return Err(format!("{} was {kept:?}, is {:?} after editing {edited}", desc.field, field(&s)));
    }
    if transient && kept.is_some() {
        return Err(format!("{} is stored", desc.field));
    }
    Ok(())
}

/// `start` (its effect at `index`) as an effect that visibly acts: as it
/// is when switching it off changes the CPU frame, else with the first of
/// its number rows, at the first of a few values, that makes it act (a
/// sample at an identity strength draws nothing to compare).
fn acting(ty: &EffectType, start: klipp_timeline::Clip, index: usize, size: (u32, u32)) -> Result<klipp_timeline::Clip, String> {
    use klipp_effects::registry::presentation::Widget;
    let differs = |c: &klipp_timeline::Clip| super::parity_pixels::cpu_frames_differ(c, &switched(c.clone(), index, false), size);
    // Strong values first, so a subtle effect survives the 8-bit GPU frame;
    // the sample as it is last.
    for p in ty.desc.params.iter().filter(|p| matches!(ty.presentation(p).widget, Widget::Scrub | Widget::Slider | Widget::Gradient(_) | Widget::Vec2)) {
        let (lo, hi) = p.soft.or(p.range).unwrap_or((0.0, 1.0));
        for share in [0.8, 0.95, 0.2, 0.05, 0.6] {
            let v = lo + (hi - lo) * share;
            let mut json = serde_json::to_value(&start.builtin_effects()[index]).map_err(|e| e.to_string())?;
            let slot = match (json.get_mut(p.field), p.index) {
                (Some(f), Some(i)) => f.get_mut(i),
                (f, None) => f,
                _ => None,
            };
            let Some(slot) = slot.filter(|f| f.is_number()) else { break };
            *slot = serde_json::json!(v);
            let Ok(effect) = serde_json::from_value::<BuiltinEffect>(json) else { break };
            let mut c = start.clone();
            let id = c.effect_id_at(index).ok_or("the effect")?;
            c.replace_effect(id, effect);
            // A value that throws the picture out of the frame draws a flat
            // one, which isn't evidence: the next is tried.
            if differs(&c).unwrap_or(false) {
                return Ok(c);
            }
        }
    }
    if differs(&start).unwrap_or(false) {
        return Ok(start);
    }
    Err("no value of its number rows makes the effect act on the frame".into())
}

pub(super) fn run(report: &mut Report) {
    // Opened: each card drawn, writing nothing, for every entry of it these
    // cases hold (its eye, its Mix, a hidden field, a workflow).
    let mut opened: Vec<(&'static EffectType, Vec<CoverageKey>)> = Vec::new();
    let mut add = |ty: &'static EffectType, key: CoverageKey| match opened.iter_mut().find(|(t, _)| t.tag == ty.tag) {
        Some((_, keys)) => keys.push(key),
        None => opened.push((ty, vec![key])),
    };
    for (e, ty) in eyes() {
        add(ty, CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, "sample", Interaction::OpenWithoutWrite));
    }
    for (e, ty) in entries("@mix") {
        add(ty, CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, NEW_ONLY, Interaction::OpenWithoutWrite));
    }
    for (e, ty, _) in hidden() {
        add(ty, CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, "sample", Interaction::OpenWithoutWrite));
    }
    for e in workflows() {
        if let Some(ty) = effect_types().find(|t| t.desc.id == e.type_id) {
            add(ty, CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, "sample", Interaction::OpenWithoutWrite));
        }
    }
    for (ty, keys) in opened {
        report.case(&format!("{} card opened", ty.tag), keys, || {
            let mut s = Stack::new(eye_fixture(&(ty.sample)()));
            let before = s.picture();
            s.frame(Vec::new());
            s.frame(Vec::new());
            if !s.unapplied.is_empty() || s.picture() != before || s.state.active_history().undo_count() != 0 {
                return Err(format!("opening wrote: {:?}", s.unapplied));
            }
            Ok(())
        });
    }
    for (entry, ty, desc) in hidden() {
        let key = CoverageKey::new(entry.type_id, &entry.parameter, Surface::EffectsStack, "sample", Interaction::PreserveHidden);
        report.case(&format!("{} keeps {}", ty.tag, desc.key), [key], || hidden_case(ty, desc));
    }
    for (entry, ty) in eyes() {
        let key = |i| CoverageKey::new(entry.type_id, &entry.parameter, Surface::EffectsStack, "sample", i);
        let effect = (ty.sample)();
        for on in [false, true] {
            let name = format!("{} eye switched {}", ty.tag, if on { "on" } else { "off" });
            report.case(&name, [key(Interaction::Toggle(on)), key(Interaction::UndoRedo)], || eye_case(&effect, on));
        }
        if eye_pixels_pinned(ty.tag).is_none() {
            match super::parity_numeric::routes_for(ty.desc.id) {
                (_, Some(reason)) => report.case_cpu_by_design(&format!("{} eye drawn", ty.tag), [key(Interaction::Pixels)], reason, || eye_pixels(ty)),
                (_, None) => report.case_or_blocked(&format!("{} eye drawn", ty.tag), [key(Interaction::Pixels)], || eye_pixels(ty)),
            }
        }
    }
    for (entry, ty) in entries("@mix") {
        let key = |i| CoverageKey::new(entry.type_id, &entry.parameter, Surface::EffectsStack, NEW_ONLY, i);
        let effect = super::parity_numeric::acting_sample(ty);
        report.case(&format!("{} Mix dragged", ty.tag), [key(Interaction::Drag), key(Interaction::UndoRedo)], || {
            mix_case(&effect, ty, |s| drag_mix(s, false)).map(drop)
        });
        report.case(&format!("{} Mix typed", ty.tag), [key(Interaction::TypeNumber)], || {
            let mix = mix_case(&effect, ty, |s| type_mix(s, "0.4"))?;
            if mix.is_none_or(|m| (m - 0.4).abs() > 1e-4) {
                return Err(format!("typed 0.4, stored {mix:?}"));
            }
            Ok(())
        });
        report.case(&format!("{} Mix reset", ty.tag), [key(Interaction::Reset)], || {
            // From a half mix, its label clicked: full again (unset), one
            // step, against the same reset by path.
            let mut from = Stack::new(fixture(&effect));
            let path = effect_path(from.clip(), ty)?;
            from.apply(&AppCommand::SetMixAt { addr: path, mix: Some(0.5) });
            let start = from.clip().clone();
            let mix = {
                let mut new = Stack::new(start.clone());
                let before = new.picture();
                let (_, label) = mix_row(&mut new)?;
                new.frame(vec![egui::Event::PointerMoved(label), press(label, true)]);
                new.frame(vec![egui::Event::PointerMoved(label), press(label, false)]);
                new.frame(Vec::new());
                if !new.unapplied.is_empty() || new.state.active_history().undo_count() != 1 {
                    return Err(format!("{} steps, unapplied {:?}", new.state.active_history().undo_count(), new.unapplied));
                }
                let mut oracle = Stack::new(start.clone());
                oracle.apply(&AppCommand::SetMixAt { addr: effect_path(&start, ty)?, mix: None });
                if new.picture() != oracle.picture() {
                    return Err("not the path op's reset".into());
                }
                let mix = mix_of(&new, ty);
                new.step(false)?;
                if new.picture() != before {
                    return Err("undo doesn't give back the half mix".into());
                }
                mix
            };
            if mix.is_some() {
                return Err(format!("reset to {mix:?}, not full"));
            }
            Ok(())
        });
        report.case(&format!("{} Mix drag cancelled", ty.tag), [key(Interaction::CancelDrag)], || {
            let mut s = Stack::new(fixture(&effect));
            let before = s.picture();
            drag_mix(&mut s, true)?;
            if !s.unapplied.is_empty() || s.picture() != before || s.state.active_history().undo_count() != 0 {
                return Err(format!("Escape left the drag: {} steps, unapplied {:?}", s.state.active_history().undo_count(), s.unapplied));
            }
            Ok(())
        });
        report.case_or_blocked(&format!("{} Mix drawn", ty.tag), [key(Interaction::Pixels)], || {
            let (start, index) = super::parity_numeric::pixel_fixture(ty, &effect, Surface::EffectsStack)?;
            let (routes, size) = routes(ty);
            let start = acting(ty, start, index, size)?;
            let mut new = Stack::new(start.clone());
            drag_mix(&mut new, false)?;
            let mut oracle = Stack::new(start.clone());
            oracle.apply(&AppCommand::SetMixAt { addr: effect_path(&start, ty)?, mix: mix_of(&new, ty) });
            super::parity_pixels::same_edit_on(routes, size, &start, oracle.clip(), new.clip())
        });
    }
    // A type that takes no mix: no Mix on its card, and a mix sent by path
    // is refused, nothing changed.
    report.case("invert takes no mix", [], || {
        let ty = effect_types().find(|t| t.tag == "invert").ok_or("no Invert type")?;
        let clip = fixture(&(ty.sample)());
        let mut s = Stack::new(clip.clone());
        // Its one "Mix" is its own parameter's row; the graph's would be a
        // second.
        let mixes = s.frame(Vec::new()).iter().filter(|(t, _)| t == "Mix").count();
        if mixes != 1 {
            return Err(format!("Invert's card draws {mixes} rows named Mix, not its own alone"));
        }
        let before = s.picture();
        s.apply(&AppCommand::SetMixAt { addr: effect_path(&clip, ty)?, mix: Some(0.5) });
        if s.unapplied.is_empty() || s.picture() != before || s.state.active_history().undo_count() != 0 {
            return Err(format!("a mix on Invert wasn't refused: {:?}", s.unapplied));
        }
        Ok(())
    });
}

/// Every eye and Mix on the stack, as above: every case passes, and every
/// key it's required for is earned, but those excluded with a citation.
#[test]
fn effect_eyes_and_mixes_store_what_they_name() {
    let mut report = Report::default();
    report.family("effect switches and mixes", run);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let missing: Vec<_> = manifest.required.iter().filter(|k| !manifest.excluded_keys.contains_key(*k) && !report.passed.contains(k) && !report.blocked.contains_key(*k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
    assert!(report.passed.iter().all(|k| manifest.required.contains(k)), "earned a key not required");
}
