//! Effect-row keyframe controls of the inspector, driven through their
//! actual diamonds, numbers and menus. The expected result is the key edit
//! made on the clip's lane directly; comparison includes the whole picture,
//! not only the changed key. Pixels and the
//! curve-editor surface belong to separate families.

use super::parity_harness::Run;
use super::parity_manifest::{
    catalog, CatalogEntry, CatalogValue, CoverageKey, Interaction, KeyInterpolation, Manifest,
    Report, Surface,
};
use super::parity_numeric::{
    editable, fixture_on, numeric, ready, type_value, RowLabel,
};
use crate::inspector::generated::row;
use klipp_core::{Interpolation, Keyframe, Timecode};
use klipp_effects::registry::{effect_types, ParamDesc};
use klipp_timeline::test_clip;

pub(super) fn eligible(entry: &CatalogEntry) -> bool {
    numeric(entry)
        && matches!(entry.value, CatalogValue::Parameter { desc, .. } if desc.keyframable)
}

pub(super) fn variant(entry: &CatalogEntry) -> &'static str {
    if entry.type_id == "klipp.fx.opacity" {
        "non-anchor"
    } else {
        "editable"
    }
}

pub(super) fn require(manifest: &mut Manifest) {
    for entry in catalog().into_iter().filter(eligible) {
        manifest.require_fixture(&entry, Surface::EffectsStack, variant(&entry));
    }
}

fn key(entry: &CatalogEntry, action: Interaction) -> CoverageKey {
    CoverageKey::new(
        entry.type_id,
        &entry.parameter,
        Surface::EffectsStack,
        variant(entry),
        action,
    )
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Action {
    Add,
    Remove,
    Edit,
    Previous,
    Next,
    Interpolate(Interpolation),
}

impl Action {
    pub(super) const ALL: [Action; 8] = [
        Action::Add,
        Action::Remove,
        Action::Edit,
        Action::Previous,
        Action::Next,
        Action::Interpolate(Interpolation::Linear),
        Action::Interpolate(Interpolation::Step),
        Action::Interpolate(Interpolation::Bezier),
    ];

    pub(super) fn interaction(self) -> Interaction {
        match self {
            Self::Add => Interaction::AddKey,
            Self::Remove => Interaction::RemoveKey,
            Self::Edit => Interaction::EditKey,
            Self::Previous => Interaction::PreviousKey,
            Self::Next => Interaction::NextKey,
            Self::Interpolate(i) => Interaction::Interpolation(match i {
                Interpolation::Linear => KeyInterpolation::Linear,
                Interpolation::Step => KeyInterpolation::Step,
                Interpolation::Bezier => KeyInterpolation::Bezier,
            }),
        }
    }
}

fn diamond(run: &mut Run, label: RowLabel) -> Result<egui::Pos2, String> {
    let (at, _) = ready(run, label)?;
    run.shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Path(path) if path.closed && path.points.len() == 4 => {
                let bounds = egui::Rect::from_points(&path.points);
                ((bounds.width() - 9.0).abs() < 0.1
                    && (bounds.height() - 9.0).abs() < 0.1
                    && (bounds.center().y - at.y).abs() < 5.0
                    && bounds.center().x > at.x)
                    .then_some(bounds.center())
            }
            _ => None,
        })
        .next()
        .ok_or_else(|| format!("no drawn diamond for {label:?}"))
}

fn click(run: &mut Run, at: egui::Pos2, button: egui::PointerButton) {
    run.button(at, button, true);
    run.button(at, button, false);
    run.frame(Vec::new(), egui::Modifiers::NONE);
}

fn navigate(run: &mut Run, label: RowLabel, next: bool) -> Result<(), String> {
    let at = diamond(run, label)?;
    // Read the painted eight-pixel caret, rather than assuming the row's
    // width or the diamond's position. Reset/behaviour icons are larger.
    let target = run
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Rect(shape) if shape.fill_texture_id != egui::TextureId::default() => {
                let rect = shape.rect;
                ((rect.width() - 8.0).abs() < 0.1
                    && (rect.height() - 8.0).abs() < 0.1
                    && (rect.center().y - at.y).abs() < 5.0
                    && (rect.center().x > at.x) == next
                    && (rect.center().x - at.x).abs() < 40.0)
                    .then_some(rect.center())
            }
            _ => None,
        })
        .min_by(|a, b| (a.x - at.x).abs().total_cmp(&(b.x - at.x).abs()))
        .ok_or_else(|| {
            format!(
                "no {} caret for {label:?}",
                if next { "next" } else { "previous" }
            )
        })?;
    click(run, target, egui::PointerButton::Primary);
    Ok(())
}

fn interpolation(run: &mut Run, label: RowLabel, want: Interpolation) -> Result<(), String> {
    let at = diamond(run, label)?;
    click(run, at, egui::PointerButton::Secondary);
    let name = match want {
        Interpolation::Linear => "Linear",
        Interpolation::Step => "Step (Hold)",
        Interpolation::Bezier => "Bezier (Custom)",
    };
    let text = run
        .frame(Vec::new(), egui::Modifiers::NONE)
        .into_iter()
        // The menu is drawn last: the Video tab shows "Linear" elsewhere.
        .rev()
        .find(|(s, _)| s.trim().trim_start_matches('*').trim() == name)
        .map(|(s, _)| s)
        .ok_or_else(|| format!("no interpolation menu item {name}"))?;
    if !run.click_last(&text) {
        return Err(format!("could not select {text:?}"));
    }
    Ok(())
}

fn edit_keys(
    clip: &mut klipp_timeline::Clip,
    surface: Surface,
    desc: &ParamDesc,
    edit: impl FnOnce(&mut Vec<Keyframe>),
) {
    // The Video tab's Opacity row keys the clip's opacity lane: the
    // anchor Opacity's, as the Video tab and curve editor show it.
    if surface == Surface::VideoTab {
        clip.edit_video_param_keyframes(klipp_core::VideoParam::Opacity, edit);
        return;
    }
    let id = test_clip::effect_id(clip, 0);
    clip.edit_effect_keyframes_of(id, desc.key, edit);
}

fn first_difference(
    actual: &serde_json::Value,
    expected: &serde_json::Value,
    path: &str,
) -> String {
    match (actual, expected) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                if a.get(key) != b.get(key) {
                    return first_difference(&a[key], &b[key], &format!("{path}/{key}"));
                }
            }
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) if a.len() == b.len() => {
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                if a != b {
                    return first_difference(a, b, &format!("{path}/{i}"));
                }
            }
        }
        _ => {}
    }
    format!("{path}: actual={actual}, expected={expected}")
}

fn exercise_case(entry: &CatalogEntry, action: Action) -> Result<(), String> {
    exercise_on(entry, Surface::EffectsStack, action)
}

/// `action` on `entry`'s row on `surface` (the Effects stack, or the
/// Video tab's anchor Opacity), held to the key edit made on the lane.
pub(super) fn exercise_on(entry: &CatalogEntry, surface: Surface, action: Action) -> Result<(), String> {
    let CatalogValue::Parameter {
        desc,
        presentation: pres,
    } = &entry.value
    else {
        unreachable!()
    };
    let ty = effect_types().find(|t| t.desc.id == entry.type_id).unwrap();
    let effect = editable(ty, pres)?;
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
    let value = |part| {
        let text = format!("{:.*}", usize::from(pres.decimals), lo + (hi - lo) * part);
        let shown = text.parse::<f32>().unwrap();
        let stored = pres
            .display
            .map_or(shown, |map| (map.to_stored)(shown, encoding));
        (text, stored)
    };
    let (_, a) = value(0.2);
    let (_, b) = value(0.8);
    let (typed, target) = value(0.6);
    let mut clip = fixture_on(&effect, surface);
    let initial_interp = if matches!(action, Action::Interpolate(Interpolation::Linear)) {
        Interpolation::Step
    } else {
        Interpolation::Linear
    };
    edit_keys(&mut clip, surface, desc, |keys| {
        *keys = [(0.25, a), (1.25, b)]
            .into_iter()
            .map(|(time_secs, value)| Keyframe {
                time_secs,
                value,
                interpolation: initial_interp,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
                handles: Default::default(),
            })
            .collect();
    });
    let before = clip.picture_v2()?;
    let at = if matches!(action, Action::Add | Action::Previous | Action::Next) {
        0.75
    } else {
        0.25
    };
    let mut oracle = clip.clone();
    let undo_steps = 1;
    edit_keys(&mut oracle, surface, desc, |keys| match action {
        Action::Add => {
            let evaluated = klipp_core::keyframe::evaluate(keys, at, a);
            klipp_core::keyframe::upsert_keyframe(keys, at, evaluated);
        }
        Action::Remove => klipp_core::keyframe::remove_keyframe_at(keys, at),
        Action::Edit => klipp_core::keyframe::upsert_keyframe(keys, at, target),
        Action::Interpolate(i) => klipp_core::keyframe::set_interpolation_at(keys, 0, i),
        Action::Previous | Action::Next => {}
    });
    let expected = oracle.picture_v2()?;
    let mut run = Run::new(clip.clone());
    run.property = surface == Surface::VideoTab;
    run.state.playhead = Timecode::from_secs_f64(at, clip.timeline_in.rate);
    match action {
        Action::Add | Action::Remove => {
            let at = diamond(&mut run, label)?;
            click(&mut run, at, egui::PointerButton::Primary);
        }
        Action::Edit => type_value(&mut run, label, &typed)?,
        Action::Previous => navigate(&mut run, label, false)?,
        Action::Next => navigate(&mut run, label, true)?,
        Action::Interpolate(i) => interpolation(&mut run, label, i)?,
    }
    if !run.unapplied.is_empty() {
        return Err(format!(
            "unapplied {:?}",
            run.unapplied
        ));
    }
    let after = run.clip().picture_v2()?;
    if after != expected {
        let difference = first_difference(
            &serde_json::to_value(&after.picture).unwrap(),
            &serde_json::to_value(&expected.picture).unwrap(),
            "picture",
        );
        return Err(format!(
            "{action:?} differs from the whole-picture lane oracle: {difference}; undo={}",
            run.state.active_history().undo_count()
        ));
    }
    if matches!(action, Action::Previous | Action::Next) {
        let target = if matches!(action, Action::Previous) {
            0.25
        } else {
            1.25
        };
        let want = Timecode::from_secs_f64(target, clip.timeline_in.rate);
        if run.seeks != [want]
            || run.state.playhead != want
            || run.state.active_history().undo_count() != 0
        {
            return Err(format!(
                "navigation {:?}, playhead {:?}, undo {}",
                run.seeks,
                run.state.playhead,
                run.state.active_history().undo_count()
            ));
        }
    } else {
        if run.state.active_history().undo_count() != undo_steps || run.state.any_drag_in_progress()
        {
            return Err(format!(
                "history {}, drag {}",
                run.state.active_history().undo_count(),
                run.state.any_drag_in_progress()
            ));
        }
        for _ in 0..undo_steps {
            if !run.step(false) {
                return Err("missing undo step".into());
            }
        }
        if run.clip().picture_v2()? != before {
            return Err("undo is not exact".into());
        }
        for _ in 0..undo_steps {
            if !run.step(true) {
                return Err("missing redo step".into());
            }
        }
        if run.clip().picture_v2()? != after {
            return Err("redo is not exact".into());
        }
    }
    Ok(())
}

pub(super) fn run(report: &mut Report) {
    for entry in catalog().into_iter().filter(eligible) {
        for action in Action::ALL {
            let name = format!("{} / {} / {action:?}", entry.type_id, entry.parameter);
            // The central rules require only Step for Whole parameters.
            // Their real menus also offer Linear/Bezier: still verify those
            // edits, but do not invent coverage keys outside the manifest.
            let extra_whole_menu = matches!(entry.value, CatalogValue::Parameter { desc, .. } if desc.kind == klipp_effects::registry::ParamKind::Whole)
                && matches!(
                    action,
                    Action::Interpolate(Interpolation::Linear | Interpolation::Bezier)
                );
            let earned = (!extra_whole_menu).then(|| key(&entry, action.interaction()));
            report.case(&name, earned, || {
                exercise_case(&entry, action)
            });
        }
    }
}

#[test]
fn numeric_effect_key_controls_preserve_the_lane_and_undo_contracts() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    run(&mut report);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty());
    assert!(report
        .passed
        .iter()
        .all(|key| manifest.required.contains(key)));
}
