//! Numeric effect controls of the inspector driven through real pointer
//! and key events. The oracle is the document command with just the
//! addressed field set to what the edit must store (the typed value, the
//! drag's rule, the reset value); comparison includes the complete
//! picture and exact undo/redo.
//! Keyframe evidence is a separate obligation. Pixels are a case of their
//! own (`pixel_case`): the typed edit drawn by the real renderer on the
//! CPU and the native GPU stack (`parity_pixels`). A type whose
//! production route is the CPU by design is drawn on the CPU alone,
//! declared in [`CPU_BY_DESIGN`] with the M4 step that moves it. One that
//! should be native and leaves the GPU route is BLOCKED with its reason,
//! open in the report and pinned in [`BLOCKED`], so neither set can change
//! unseen.

use super::parity_harness::{fixture, Run};
use super::parity_manifest::{
    catalog, CatalogEntry, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface,
};
use crate::inspector::generated::{effect_value, row};
use klipp_core::AppCommand;
use klipp_effects::registry::presentation::{Cond, Presentation, Widget};
use klipp_effects::registry::{effect_types, EffectType, ParamDesc, ParamKind};
use klipp_effects::BuiltinEffect;
use serde_json::Value;

const VARIANT: &str = "editable";

pub(super) fn numeric(entry: &CatalogEntry) -> bool {
    !entry.grade
        && matches!(entry.value, CatalogValue::Parameter { presentation, .. }
        if matches!(presentation.widget, Widget::Scrub | Widget::Slider | Widget::Gradient(_) | Widget::HueRainbow | Widget::Vec2))
}

fn contexts(ty: &EffectType) -> Vec<(Surface, &'static str)> {
    // A lone opacity node is the clip anchor, hidden from the stack.
    // Other placed kinds still draw their own rows in Effects/Placement.
    if ty.tag == "opacity" {
        vec![
            (Surface::VideoTab, "anchor"),
            (Surface::EffectsStack, "non-anchor"),
        ]
    } else {
        vec![(Surface::EffectsStack, VARIANT)]
    }
}

/// Rows whose control is shown but doesn't move the picture, or whose
/// picture the CPU and the GPU stack draw differently: known defects, each
/// excluded from Pixels with its issue until it is fixed (type, parameter,
/// surface, variant, reason). Not a route, and not a
/// design: the deletion report reads them as open gaps.
const KNOWN_DEFECTS: &[(&str, &str, Surface, &str, &str)] = &[
    ("klipp.fx.transform", "scale_y", Surface::EffectsStack, VARIANT, "known defect #1406: placement renders a uniform scale from scale_x, so the Scale Y row moves nothing"),
    ("klipp.fx.opacity", "value", Surface::EffectsStack, "non-anchor", "known defect #1386: a non-anchor Opacity entry is kept off, so its value moves nothing"),
    ("klipp.fx.speed_ramp", "speed", Surface::EffectsStack, VARIANT, "known defect #1409: retiming comes from the clip's speed curve, so the effect's Speed moves nothing"),
    ("klipp.fx.speed_ramp", "ramp_start", Surface::EffectsStack, VARIANT, "known defect #1409: the lift drops the ramp, so Ramp Start moves nothing"),
    ("klipp.fx.speed_ramp", "ramp_end", Surface::EffectsStack, VARIANT, "known defect #1409: the lift drops the ramp, so Ramp End moves nothing"),
    ("klipp.fx.speed_ramp", "freeze_at", Surface::EffectsStack, VARIANT, "known defect #1409: the lift drops the ramp, so Freeze At moves nothing"),
];

pub(super) fn known_defect(entry: &CatalogEntry, surface: Surface, variant: &str) -> Option<&'static str> {
    KNOWN_DEFECTS.iter().find(|(ty, param, s, v, _)| *ty == entry.type_id && *param == entry.parameter && *s == surface && *v == variant).map(|d| d.4)
}

pub(super) fn require(manifest: &mut Manifest) {
    for entry in catalog().into_iter().filter(numeric) {
        let ty = effect_types()
            .find(|ty| ty.desc.id == entry.type_id)
            .unwrap();
        for (surface, variant) in contexts(ty) {
            manifest.require_fixture(&entry, surface, variant);
            if let Some(reason) = known_defect(&entry, surface, variant) {
                manifest.exclude_key(CoverageKey::new(entry.type_id, &entry.parameter, surface, variant, Interaction::Pixels), reason);
            }
        }
    }
}

pub(super) fn with_field(
    effect: &BuiltinEffect,
    desc: &ParamDesc,
    value: Value,
) -> Result<BuiltinEffect, String> {
    let mut entry = serde_json::to_value(effect).map_err(|e| e.to_string())?;
    match desc.index {
        Some(index) => entry[desc.field][index] = value,
        None => entry[desc.field] = value,
    }
    serde_json::from_value(entry).map_err(|e| format!("{}: {e}", desc.key))
}

pub(super) fn stored(effect: &BuiltinEffect, desc: &ParamDesc) -> Value {
    effect_value(&serde_json::to_value(effect).unwrap(), desc)
}

pub(super) fn number(desc: &ParamDesc, value: f32) -> Value {
    if desc.kind == ParamKind::Whole {
        Value::from(value.round().max(0.0) as u32)
    } else {
        Value::from(value)
    }
}

pub(super) fn editable(ty: &EffectType, pres: &Presentation) -> Result<BuiltinEffect, String> {
    let mut effect = (ty.sample)();
    for Cond::ParamIs { key, value } in [pres.visible_if, pres.enabled_if].into_iter().flatten() {
        let desc = ty
            .desc
            .params
            .iter()
            .find(|p| p.key == key)
            .ok_or_else(|| format!("unknown condition {key}"))?;
        let value = match (desc.kind, stored(&effect, desc)) {
            (ParamKind::Choice(names), Value::String(_)) => Value::from(
                *names
                    .get(value as usize)
                    .ok_or("condition index outside choices")?,
            ),
            (_, Value::Bool(_)) => Value::Bool(value != 0.0),
            (_, Value::Number(n)) if n.is_f64() => Value::from(value),
            _ => Value::from(value as u32),
        };
        effect = with_field(&effect, desc, value)?;
    }
    Ok(effect)
}

pub(super) fn fixture_on(effect: &BuiltinEffect, surface: Surface) -> klipp_timeline::Clip {
    let mut clip = fixture(effect);
    if matches!(effect, BuiltinEffect::Opacity { .. }) && surface == Surface::EffectsStack {
        // The final enabled Opacity becomes the clip anchor. The first
        // remains a card and must be exercised independently.
        clip.push_effect(BuiltinEffect::Opacity { value: 1.0 });
    }
    clip
}

fn run_for(surface: Surface, clip: klipp_timeline::Clip) -> Run {
    let mut run = Run::new(clip);
    run.property = surface == Surface::VideoTab;
    run
}

#[derive(Clone, Copy, Debug)]
pub(super) struct RowLabel {
    pub(super) text: &'static str,
    pub(super) group: Option<&'static str>,
}

pub(super) fn ready(run: &mut Run, label: RowLabel) -> Result<(egui::Pos2, egui::Pos2), String> {
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let Some(group) = label.group else {
        return run
            .row(label.text)
            .ok_or_else(|| format!("numeric row {label:?} is not drawn"));
    };
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    let group_y = drawn
        .iter()
        .find(|(s, _)| s == group)
        .map(|(_, p)| p.y)
        .ok_or_else(|| format!("group {group:?} is not drawn"))?;
    let at = drawn
        .iter()
        .filter(|(s, p)| s == label.text && p.y > group_y)
        .min_by(|a, b| a.1.y.total_cmp(&b.1.y))
        .map(|(_, p)| *p)
        .ok_or_else(|| format!("numeric row {label:?} is not drawn"))?;
    drawn
        .iter()
        .filter(|(s, p)| {
            (p.y - at.y).abs() < 6.0
                && p.x > at.x
                && s.trim_start_matches('-')
                    .starts_with(|c: char| c.is_ascii_digit())
        })
        .min_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .map(|(_, p)| (at, *p))
        .ok_or_else(|| format!("numeric value {label:?} is not drawn"))
}

pub(super) fn type_value(run: &mut Run, label: RowLabel, text: &str) -> Result<(), String> {
    let (_, at) = ready(run, label)?;
    for _ in 0..2 {
        run.button(at, egui::PointerButton::Primary, true);
        run.button(at, egui::PointerButton::Primary, false);
    }
    run.frame(Vec::new(), egui::Modifiers::NONE);
    run.key(egui::Key::A, egui::Modifiers::COMMAND);
    run.frame(vec![egui::Event::Text(text.into())], egui::Modifiers::NONE);
    run.key(egui::Key::Enter, egui::Modifiers::NONE);
    Ok(())
}

/// How far a row is dragged, in one move: 64 pixels, or for a whole-number
/// row as far as moves it a unit and a half (within the harness's screen),
/// since a shorter drag rounds back to where it was.
fn drag_px(desc: &ParamDesc, pres: &Presentation) -> f32 {
    match desc.kind {
        ParamKind::Whole => (1.5 / row::drag_speed(pres.step)).ceil().clamp(64.0, 400.0) as f32,
        _ => 64.0,
    }
}

/// A drag of the row by `px` in one move, held: a scrubby row's by its
/// label, its own drag, whose one move is one step of its rule
/// (`value + delta · speed`); a slider row's by its number, as its label
/// only resets. Returns where the pointer is.
fn start_drag(run: &mut Run, label: RowLabel, px: f32, slider: bool) -> Result<egui::Pos2, String> {
    let (on_label, on_number) = ready(run, label)?;
    let at = if slider { on_number } else { on_label };
    run.button(at, egui::PointerButton::Primary, true);
    run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(px, 0.0))], egui::Modifiers::NONE);
    Ok(at + egui::vec2(px, 0.0))
}

fn commands_applied(run: &Run) -> Result<(), String> {
    if run.unapplied.is_empty() {
        Ok(())
    } else {
        Err(format!("unapplied: {:?}", run.unapplied))
    }
}

/// The analysis a row asks the app to redo when it's edited, if any, and
/// whether it asks only once the clip is measured: Deflicker's window
/// always asks for its gains again; Stabilisation's smoothing has a
/// trajectory to smooth only once measured.
fn analysis_request(ty: &EffectType, desc: &ParamDesc) -> Option<(&'static str, bool)> {
    match (ty.tag, desc.key) {
        ("deflicker", "window_secs") => Some(("RegainDeflicker", false)),
        ("video_stabilization", "smoothing") => Some(("ResmoothStabilization", true)),
        _ => None,
    }
}

fn edit_case(
    ty: &EffectType,
    surface: Surface,
    effect: &BuiltinEffect,
    desc: &ParamDesc,
    want: Option<Value>,
    drive: impl Fn(&mut Run) -> Result<(), String>,
) -> Result<(), String> {
    let clip = fixture_on(effect, surface);
    let side_clip = clip.clone();
    let before = clip.picture_v2().unwrap();
    // The view driven, and the oracle: the document command alone,
    // applied to the same clip, never a view.
    let (mut new, mut oracle) = (
        run_for(surface, clip.clone()),
        run_for(surface, clip),
    );
    drive(&mut new)?;
    commands_applied(&new)?;
    let after = new.clip().picture_v2().unwrap();
    if after == before {
        return Err("the control changed nothing".into());
    }
    let actual = new
        .clip()
        .builtin_effects()
        .first()
        .map(|effect| stored(effect, desc))
        .ok_or("effect disappeared")?;
    // A slider row places its value where the pointer is, by its own
    // layout: what it must store there is held to the drag's direction (to
    // the right, a rise) and, below, to the document command's picture.
    if want.is_none() {
        let start = stored(effect, desc).as_f64();
        if !actual.as_f64().zip(start).is_some_and(|(to, from)| to > from) {
            return Err(format!("a drag to the right stored {actual}, from {}", stored(effect, desc)));
        }
    }
    let expected = want.unwrap_or_else(|| actual.clone());
    oracle.apply(AppCommand::UpdateBuiltinEffect {
        clip_id: oracle.clip,
        index: 0,
        effect: with_field(effect, desc, expected.clone())?,
    });
    commands_applied(&oracle)?;
    if after != oracle.clip().picture_v2().unwrap() {
        return Err(format!(
            "not the document command's picture: value {actual}, expected {expected}"
        ));
    }
    // A row that asks the app to bring its analysis up to the edit sends
    // that request once, besides what the document command sends: the
    // expected side commands.
    let measured = side_clip.deflicker_data.is_some() || side_clip.stabilization_data.is_some();
    if let Some((request, _)) = analysis_request(ty, desc).filter(|(_, once_measured)| measured || !once_measured) {
        match new.side.iter().position(|sent| sent.starts_with(request)) {
            Some(i) => {
                new.side.remove(i);
            }
            None => return Err(format!("the row didn't ask for {request}: sent {:?}", new.side)),
        }
    }
    if new.side != oracle.side {
        return Err(format!(
            "side commands differ: {:?} / {:?}",
            new.side, oracle.side
        ));
    }
    if new.state.any_drag_in_progress() || new.state.active_history().undo_count() != 1 {
        return Err(format!(
            "edit has {} undo steps; drag open {}",
            new.state.active_history().undo_count(),
            new.state.any_drag_in_progress()
        ));
    }
    if !new.step(false) || new.clip().picture_v2().unwrap() != before {
        return Err("undo did not restore the exact picture".into());
    }
    if !new.step(true) || new.clip().picture_v2().unwrap() != after {
        return Err("redo did not restore the exact edit".into());
    }
    Ok(())
}

/// The effect types whose Pixels cases are BLOCKED, each with why: their
/// frame can't stay on the native GPU route, so each needs its own route
/// handling before it earns pixel evidence. The family test holds the
/// blocked set to this list exactly.
const BLOCKED: &[(&str, &str)] = &[];

/// The effect types whose production route is the CPU by design, each
/// with the M4 step that moves it: their Pixels are drawn on the CPU route
/// alone, the GPU half declared not applicable (never BLOCKED).
pub(super) const CPU_BY_DESIGN: &[(&str, &str)] = &[
    ("klipp.fx.temporal_denoise", "M4 step 6 (temporal denoise as a source-boundary node): the GPU stack declines a frame with temporal noise reduction"),
    ("klipp.fx.video_stabilization", "M4 step 9 (analysis effects as nodes) deletes the stack's temporal gate (klipp-compose render.rs), which draws a stabilised layer on the CPU"),
    ("klipp.fx.rolling_shutter_correction", "M4 step 9 (analysis effects as nodes) deletes the stack's temporal gate (klipp-compose render.rs), which draws a corrected layer on the CPU"),
];

/// What a row's pixel case starts from beyond the effect's sample, by
/// effect tag, so its edit can show in a frame: values of other fields
/// the edited one acts through, as JSON (a crop for its feather, a tilt
/// for the perspective, a visible shadow for its offset), and whether the
/// clip is placed shrunk first, for an effect drawn in the margin a
/// smaller picture leaves (a glow, a shadow, fill bars).
const PIXEL_CONTEXT: &[(&str, &[(&str, &str)], bool)] = &[
    ("crop", &[("left", "0.1"), ("right", "0.1"), ("top", "0.1"), ("bottom", "0.1")], false),
    ("rounded_corners", &[("radius", "0.2")], false),
    ("transform", &[("rotation", "20.0"), ("scale_x", "0.8"), ("scale_y", "0.8"), ("rotation_x", "20.0")], false),
    ("outer_glow", &[("radius", "0.05"), ("intensity", "0.6")], true),
    // White, so the shadow shows over the black the frame is flattened on.
    ("drop_shadow", &[("offset_x", "0.04"), ("offset_y", "0.04"), ("blur", "0.01"), ("opacity", "0.8"), ("color", "[255, 255, 255]")], true),
    ("fill_bars", &[], true),
];

/// The effect types drawn at the delivery size: their strength is scaled
/// to the frame, so the oracle's small frame rounds it away (a gate weave
/// of a few pixels at 1920 wide is under one at 320).
pub(super) const AT_FULL_SIZE: &[&str] = &["film_grain", "old_film"];

/// The routes a type's Pixels are drawn on, and the declared reason when
/// it is the CPU alone ([`CPU_BY_DESIGN`]).
pub(super) fn routes_for(type_id: &str) -> (super::parity_pixels::Routes, Option<&'static str>) {
    match CPU_BY_DESIGN.iter().find(|(ty, _)| *ty == type_id) {
        Some((_, why)) => (super::parity_pixels::Routes::CpuByDesign, Some(why)),
        None => (super::parity_pixels::Routes::Both, None),
    }
}

/// The frame size a type's Pixels are drawn at ([`AT_FULL_SIZE`]).
pub(super) fn size_for(tag: &str) -> (u32, u32) {
    if AT_FULL_SIZE.contains(&tag) { super::parity_pixels::FULL } else { super::parity_pixels::SMALL }
}

/// The clip a row's pixel case edits, and the place of the edited effect
/// in its list: `effect` with the [`PIXEL_CONTEXT`] for its tag, after a
/// shrinking Transform where the context asks for one, with the analysis
/// an analysis effect draws by.
pub(super) fn pixel_fixture(ty: &EffectType, effect: &BuiltinEffect, surface: Surface) -> Result<(klipp_timeline::Clip, usize), String> {
    let context = PIXEL_CONTEXT.iter().find(|(tag, _, _)| *tag == ty.tag);
    let mut entry = serde_json::to_value(effect).map_err(|e| e.to_string())?;
    for (field, value) in context.map_or(&[][..], |(_, fields, _)| *fields) {
        entry[*field] = serde_json::from_str(value).map_err(|e| format!("{}.{field}: {e}", ty.tag))?;
    }
    let effect: BuiltinEffect = serde_json::from_value(entry).map_err(|e| format!("{}: {e}", ty.tag))?;
    let mut clip = fixture_on(&effect, surface);
    match ty.tag {
        "deflicker" => {
            // A measured flicker, as the analysis leaves it: the gains move
            // the picture, and the strength and window move the gains.
            clip.deflicker_data = Some(klipp_effects::deflicker::DeflickerData { luma: (0..48).map(|i| 0.35 + 0.25 * ((i % 4) as f32) / 3.0).collect(), fps: 24.0 });
            regain(&mut clip);
        }
        "video_stabilization" | "rolling_shutter_correction" => {
            // A measured shake, as the analysis leaves it.
            use klipp_effects::rolling_shutter::Homography;
            use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, StabilizationData};
            // A drift with a shake on it, so the smoothing window moves
            // the correction.
            let step = |i: usize| Homography { h: [[1.0, 0.0, if i.is_multiple_of(2) { 6.0 } else { -2.0 }], [0.0, 1.0, if i.is_multiple_of(5) { 3.0 } else { 0.5 }], [0.0, 0.0, 1.0]] };
            let frame_transforms: Vec<Homography> = (0..47).map(step).collect();
            let accumulated = accumulate_trajectory(&frame_transforms);
            let trajectory = accumulated.iter().map(decompose_homography).collect();
            let data = StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 };
            clip.stabilization_data = Some(data);
            regain(&mut clip);
        }
        _ => {}
    }
    if context.is_some_and(|(_, _, shrunk)| *shrunk) {
        let shrink = BuiltinEffect::Transform { position_x: 0.0, position_y: 0.0, scale_x: 0.6, scale_y: 0.6, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        clip.insert_effect(0, shrink).ok_or("the shrinking Transform")?;
        return Ok((clip, 1));
    }
    Ok((clip, 0))
}

/// The analysis an analysis effect draws by, brought up to its settings
/// as the app does after an edit of them (the parity runs record those
/// side commands, they don't apply them): the deflicker gains for its
/// window (`RegainDeflicker`), and the stabilisation's trajectory smoothed
/// over its smoothing's window (`ResmoothStabilization`).
pub(super) fn regain(clip: &mut klipp_timeline::Clip) {
    let window = clip.builtin_effects().iter().find_map(|e| match e {
        BuiltinEffect::Deflicker { window_secs, .. } => Some(*window_secs as f64),
        _ => None,
    });
    if let (Some(window), Some(data)) = (window, clip.deflicker_data.as_ref()) {
        clip.deflicker_gains = Some(klipp_effects::deflicker::DeflickerGains::refresh(clip.deflicker_gains.clone(), data, window));
    }
    let smoothing = clip.builtin_effects().iter().find_map(|e| match e {
        BuiltinEffect::VideoStabilization { smoothing, .. } => Some(*smoothing as f64),
        _ => None,
    });
    if let (Some(smoothing), Some(data)) = (smoothing, clip.stabilization_data.as_ref()) {
        let duration_secs = data.frame_count as f64 / data.fps.max(1.0);
        let window_secs = (smoothing.clamp(0.0, 1.0) * duration_secs).max(0.05);
        clip.smoothed_trajectory = Some(klipp_effects::stabilization::smooth_trajectory(data, window_secs));
    }
}

/// A retiming fixture's positive control: the clip's own speed changed
/// must pick a different source frame at the sample time, so a ramp row
/// that moves nothing is the row's, not the fixture's.
fn retime_control(clip: &klipp_timeline::Clip, size: (u32, u32)) -> Result<(), String> {
    let mut faster = clip.clone();
    faster.speed = 1.5;
    if !super::parity_pixels::cpu_frames_differ(clip, &faster, size)? {
        return Err("positive control: the clip's speed doesn't pick another frame, so the fixture can't show a retime".into());
    }
    Ok(())
}

/// The typed edit drawn by the renderer: on the [`pixel_fixture`], the
/// generated view's row typed to the first of a few values near the
/// middle of its range whose edit (by the document command) changes the
/// CPU frame (a modest move, which keeps a moved picture in the frame),
/// drawn against the unedited clip: it changes the frame, and the CPU
/// draws it as the GPU does ([`super::parity_pixels::clip_edit_drawn_on`]).
/// A row no such value moves is an error naming it.
#[allow(clippy::too_many_arguments)]
fn pixel_case(routes: super::parity_pixels::Routes, ty: &EffectType, surface: Surface, effect: &BuiltinEffect, desc: &ParamDesc, label: RowLabel, value_at: impl Fn(f32) -> (String, Value)) -> Result<(), String> {
    use super::parity_pixels::cpu_frames_differ;
    let size = size_for(ty.tag);
    let (clip, index) = pixel_fixture(ty, effect, surface)?;
    if ty.tag == "speed_ramp" {
        retime_control(&clip, size)?;
    }
    let current = stored(&clip.builtin_effects()[index], desc);
    let edit = |target: &Value| -> Result<klipp_timeline::Clip, String> {
        let mut c = clip.clone();
        let id = c.effect_id_at(index).ok_or("the edited effect")?;
        c.replace_effect(id, with_field(&clip.builtin_effects()[index], desc, target.clone())?);
        regain(&mut c);
        Ok(c)
    };
    let mut chosen = None;
    for (typed, target) in [0.55, 0.45, 0.65, 0.35, 0.8, 0.2, 0.95, 0.05].into_iter().map(&value_at) {
        // A value whose frame can't be drawn (a move that takes the
        // picture out of the frame) is passed over for the next.
        if target != current && matches!(cpu_frames_differ(&clip, &edit(&target)?, size), Ok(true)) {
            chosen = Some((typed, target));
            break;
        }
    }
    let (typed, target) = chosen.ok_or("the edit doesn't change the CPU frame at any value tried")?;
    let start = clip.clone();
    let mut new = run_for(surface, clip);
    type_value(&mut new, label, &typed)?;
    commands_applied(&new)?;
    // The row asks the app to bring its analysis up to the edit; `regain`
    // stands in for that below, so the request must have been sent.
    if let Some((command, _)) = analysis_request(ty, desc) {
        if !new.side.iter().any(|sent| sent.starts_with(command)) {
            return Err(format!("the row didn't ask for {command}: sent {:?}", new.side));
        }
    }
    let _ = target;
    let mut new = new.clip().clone();
    regain(&mut new);
    super::parity_pixels::clip_edit_drawn_on(routes, size, &start, &new)
}

fn run_entry(report: &mut Report, entry: &CatalogEntry, surface: Surface, variant: &str) {
    let CatalogValue::Parameter {
        desc,
        presentation: pres,
    } = &entry.value
    else {
        unreachable!()
    };
    let ty = effect_types()
        .find(|ty| ty.desc.id == entry.type_id)
        .unwrap();
    let key = |interaction| {
        CoverageKey::new(
            entry.type_id,
            &entry.parameter,
            surface,
            variant,
            interaction,
        )
    };
    let effect = match editable(ty, pres) {
        Ok(effect) => effect,
        Err(error) => {
            report.case(entry.type_id, [], || Err(error));
            return;
        }
    };
    let label = RowLabel {
        text: pres.label.unwrap_or(desc.label),
        group: pres.group,
    };
    let encoding = klipp_grade_model::Encoding::from_u8(
        klipp_core::grade_encoding()
            .effective(klipp_engine::video_decoder::working_colorspace())
            .as_u8(),
    );
    let (lo, hi) = row::shown_range(desc, pres, encoding);
    let to_stored = |value| {
        pres.display
            .map_or(value, |map| (map.to_stored)(value, encoding))
    };
    let value_at = |part: f32| {
        let shown = lo + (hi - lo) * part;
        let text = format!("{:.*}", usize::from(pres.decimals), shown);
        let value = number(desc, to_stored(text.parse().unwrap()));
        (text, value)
    };
    // Stay apart after integer rounding, including a small 1..=3 range.
    let (_, start) = value_at(0.1);
    let (typed, target) = value_at(0.9);
    let editing = with_field(&effect, desc, start).unwrap();
    let name = |action| {
        format!(
            "{} / {} / {surface:?} / {variant} / {action}",
            entry.type_id, entry.parameter
        )
    };

    report.case(&name("open"), [key(Interaction::OpenWithoutWrite)], || {
        let mut run = run_for(surface, fixture_on(&effect, surface));
        let before = run.clip().picture_v2().unwrap();
        let dirty = run.state.dirty;
        ready(&mut run, label)?;
        commands_applied(&run)?;
        if run.clip().picture_v2().unwrap() != before
            || run.state.dirty != dirty
            || run.state.active_history().undo_count() != 0
        {
            return Err("opening the row wrote document state".into());
        }
        Ok(())
    });
    report.case(
        &name("type"),
        [key(Interaction::TypeNumber), key(Interaction::UndoRedo)],
        || {
            edit_case(ty, surface, &editing, desc, Some(target.clone()), |run| {
                type_value(run, label, &typed)
            })
        },
    );
    use super::parity_pixels::Routes;
    match CPU_BY_DESIGN.iter().find(|(ty, _)| *ty == entry.type_id).filter(|_| known_defect(entry, surface, variant).is_none()) {
        Some((_, reason)) => report.case_cpu_by_design(&name("pixels"), [key(Interaction::Pixels)], reason, || {
            pixel_case(Routes::CpuByDesign, ty, surface, &effect, desc, label, value_at)
        }),
        // A known CPU/GPU defect: its key excluded with the issue, the edit
        // still changes the frame.
        None if known_defect(entry, surface, variant).is_some_and(|why| why.starts_with("known CPU/GPU")) => {
            report.case(&name("pixels, routes apart (pinned)"), [], || super::parity_pixels::with_routes_apart(|| pixel_case(Routes::Both, ty, surface, &effect, desc, label, value_at)))
        }
        None if known_defect(entry, surface, variant).is_some() => {}
        None => report.case_or_blocked(&name("pixels"), [key(Interaction::Pixels)], || {
            pixel_case(Routes::Both, ty, surface, &effect, desc, label, value_at)
        }),
    }
    // What the drag must store: a scrubby row's rule from where it starts,
    // over the range it draws (its shown range, widened to hold the value
    // and the reset), whole for a whole-number row. A slider row places
    // its value where the pointer is, held to having moved instead.
    let dragged_to = match pres.widget {
        Widget::Slider => None,
        _ => stored(&editing, desc).as_f64().map(|v| {
            let (v, reset) = (v as f32, row::reset_value(desc, pres));
            let (lo, hi) = (lo.min(v).min(reset), hi.max(v).max(reset));
            number(desc, (v + (f64::from(drag_px(desc, pres)) * row::drag_speed(pres.step)) as f32).clamp(lo, hi))
        }),
    };
    report.case(
        &name("drag"),
        [key(Interaction::Drag), key(Interaction::UndoRedo)],
        || {
            edit_case(ty, surface, &editing, desc, dragged_to.clone(), |run| {
                let at = start_drag(run, label, drag_px(desc, pres), matches!(pres.widget, Widget::Slider))?;
                run.button(at, egui::PointerButton::Primary, false);
                Ok(())
            })
        },
    );
    let reset = number(desc, row::reset_value(desc, pres));
    let away = if stored(&editing, desc) == reset {
        with_field(&effect, desc, value_at(0.9).1).unwrap()
    } else {
        editing.clone()
    };
    report.case(
        &name("reset"),
        [key(Interaction::Reset), key(Interaction::UndoRedo)],
        || {
            edit_case(ty, surface, &away, desc, Some(reset.clone()), |run| {
                let (at, _) = ready(run, label)?;
                run.button(at, egui::PointerButton::Primary, true);
                run.button(at, egui::PointerButton::Primary, false);
                Ok(())
            })
        },
    );
    report.case(&name("cancel"), [key(Interaction::CancelDrag)], || {
        let mut run = run_for(surface, fixture_on(&editing, surface));
        let before = run.clip().picture_v2().unwrap();
        let at = start_drag(&mut run, label, drag_px(desc, pres), matches!(pres.widget, Widget::Slider))?;
        if run.clip().picture_v2().unwrap() == before {
            return Err("cancel setup did not change the value".into());
        }
        run.key(egui::Key::Escape, egui::Modifiers::NONE);
        run.frame(
            vec![egui::Event::PointerMoved(at + egui::vec2(10.0, 0.0))],
            egui::Modifiers::NONE,
        );
        run.button(
            at + egui::vec2(10.0, 0.0),
            egui::PointerButton::Primary,
            false,
        );
        commands_applied(&run)?;
        if run.clip().picture_v2().unwrap() != before
            || run.state.active_history().undo_count() != 0
            || run.state.any_drag_in_progress()
        {
            return Err("Escape did not restore the picture without an undo entry".into());
        }
        Ok(())
    });
}

pub(super) fn run(report: &mut Report) {
    for entry in catalog().into_iter().filter(numeric) {
        let ty = effect_types()
            .find(|ty| ty.desc.id == entry.type_id)
            .unwrap();
        for (surface, variant) in contexts(ty) {
            let name = format!(
                "{} / {} / {surface:?} / {variant}",
                entry.type_id, entry.parameter
            );
            report.family(&name, |report| run_entry(report, &entry, surface, variant));
        }
    }
}

#[test]
fn all_effect_numeric_rows_edit_and_cancel_through_real_controls() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    run(&mut report);
    eprintln!(
        "numeric catalog rows: {}; earned keys: {}; failures: {}",
        catalog().iter().filter(|e| numeric(e)).count(),
        report.passed.len(),
        report.failures.len()
    );
    for (key, why) in report.blocked.iter().chain(&report.cpu_by_design) {
        eprintln!("{key:?}: {why}");
    }
    report.family_result().unwrap();
    assert!(!report.passed.is_empty());
    assert!(report
        .passed
        .iter()
        .all(|key| manifest.required.contains(key)));
    // The blocked Pixels cases are exactly the pinned types: a new one is
    // a route regression to look at, and one that passes now comes off
    // the list.
    let blocked: std::collections::BTreeSet<&str> = report.blocked.keys().map(|k| k.type_id.as_str()).collect();
    let pinned: std::collections::BTreeSet<&str> = BLOCKED.iter().map(|(ty, _)| *ty).collect();
    assert_eq!(blocked, pinned, "blocked Pixels types differ from the pinned list");
}
