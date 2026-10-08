//! Numeric effect controls driven through real pointer/key events. The
//! oracle is the old document command with just the addressed field
//! changed; comparison includes the complete picture and exact undo/redo.
//! Keyframe and pixel evidence are separate obligations, never credited
//! by these scalar-edit checks.

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

fn numeric(entry: &CatalogEntry) -> bool {
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

pub(super) fn require(manifest: &mut Manifest) {
    for entry in catalog().into_iter().filter(numeric) {
        let ty = effect_types()
            .find(|ty| ty.desc.id == entry.type_id)
            .unwrap();
        for (surface, variant) in contexts(ty) {
            manifest.require_fixture(&entry, surface, variant);
        }
    }
}

fn with_field(
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

fn stored(effect: &BuiltinEffect, desc: &ParamDesc) -> Value {
    effect_value(&serde_json::to_value(effect).unwrap(), desc)
}

fn number(desc: &ParamDesc, value: f32) -> Value {
    if desc.kind == ParamKind::Whole {
        Value::from(value.round().max(0.0) as u32)
    } else {
        Value::from(value)
    }
}

fn editable(ty: &EffectType, pres: &Presentation) -> Result<BuiltinEffect, String> {
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

fn fixture_on(effect: &BuiltinEffect, surface: Surface) -> klipp_timeline::Clip {
    let mut clip = fixture(effect);
    if matches!(effect, BuiltinEffect::Opacity { .. }) && surface == Surface::EffectsStack {
        // The final enabled Opacity becomes the clip anchor. The first
        // remains a card and must be exercised independently.
        clip.push_effect(BuiltinEffect::Opacity { value: 1.0 });
    }
    clip
}

fn run_for(surface: Surface, clip: klipp_timeline::Clip, generated: bool) -> Run {
    let mut run = Run::new(clip, generated);
    run.property = surface == Surface::VideoTab;
    run
}

#[derive(Clone, Copy, Debug)]
struct RowLabel {
    text: &'static str,
    group: Option<&'static str>,
}

fn ready(run: &mut Run, label: RowLabel) -> Result<(egui::Pos2, egui::Pos2), String> {
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

fn type_value(run: &mut Run, label: RowLabel, text: &str) -> Result<(), String> {
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

fn start_drag(run: &mut Run, label: RowLabel) -> Result<egui::Pos2, String> {
    let (_, at) = ready(run, label)?;
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [12.0, 32.0, 64.0] {
        run.frame(
            vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))],
            egui::Modifiers::NONE,
        );
    }
    Ok(at + egui::vec2(64.0, 0.0))
}

fn commands_applied(run: &Run) -> Result<(), String> {
    if run.unapplied.is_empty() {
        Ok(())
    } else {
        Err(format!("unapplied: {:?}", run.unapplied))
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
    let (mut new, mut old) = (
        run_for(surface, clip.clone(), true),
        run_for(surface, clip, false),
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
    let expected = want.unwrap_or_else(|| actual.clone());
    old.apply(AppCommand::UpdateBuiltinEffect {
        clip_id: old.clip,
        index: 0,
        effect: with_field(effect, desc, expected.clone())?,
    });
    commands_applied(&old)?;
    if after != old.clip().picture_v2().unwrap() {
        return Err(format!(
            "not the old command's picture: value {actual}, expected {expected}"
        ));
    }
    // Deflicker's row schedules analysis after changing its window. A
    // bare document command does not; exercise the actual old row for
    // that side-effect oracle, keeping the exact document oracle above.
    if ty.tag == "deflicker" && desc.key == "window_secs" {
        let mut side_oracle = run_for(surface, side_clip, false);
        drive(&mut side_oracle)?;
        commands_applied(&side_oracle)?;
        old.side = side_oracle.side;
    }
    if new.side != old.side {
        return Err(format!(
            "side commands differ: {:?} / {:?}",
            new.side, old.side
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
        let mut run = run_for(surface, fixture_on(&effect, surface), true);
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
    report.case(
        &name("drag"),
        [key(Interaction::Drag), key(Interaction::UndoRedo)],
        || {
            edit_case(ty, surface, &editing, desc, None, |run| {
                let at = start_drag(run, label)?;
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
        let mut run = run_for(surface, fixture_on(&editing, surface), true);
        let before = run.clip().picture_v2().unwrap();
        let at = start_drag(&mut run, label)?;
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
    report.family_result().unwrap();
    assert!(!report.passed.is_empty());
    assert!(report
        .passed
        .iter()
        .all(|key| manifest.required.contains(key)));
}
