//! Video Transform controls are a separate surface: their Scale writes
//! both axes and Perspective resets to the displayed default of one.
//! Each edit is also drawn by the real renderer (`parity_pixels`) on the
//! CPU and the native GPU stack; these cases earn no keyframe evidence.
//! The tab's Opacity row is the clip's anchor Opacity: its diamond, carets
//! and menu are driven by `parity_effect_keys`' driver on the Video tab
//! (`anchor_opacity_keys`), its numbers by `parity_numeric`.

use super::parity_harness::{fixture, Run};
use super::parity_manifest::{catalog, CoverageKey, Interaction, Manifest, Report, Surface};
use crate::inspector::video_tab::TransformParams;

const TYPE: &str = "klipp.fx.transform";
const VARIANT: &str = "tilted";

#[derive(Clone, Copy)]
struct Control {
    keys: &'static [&'static str],
    label: &'static str,
    typed: &'static str,
    reset: f32,
}

const CONTROLS: &[Control] = &[
    Control {
        keys: &["position_x"],
        label: "Position X",
        typed: "80",
        reset: 0.0,
    },
    Control {
        keys: &["position_y"],
        label: "Position Y",
        typed: "90",
        reset: 0.0,
    },
    Control {
        keys: &["scale_x", "scale_y"],
        label: "Scale",
        typed: "1.80",
        reset: 1.0,
    },
    Control {
        keys: &["rotation"],
        label: "Rotation",
        typed: "25.0",
        reset: 0.0,
    },
    Control {
        keys: &["rotation_x"],
        label: "Tilt X",
        typed: "20.0",
        reset: 0.0,
    },
    Control {
        keys: &["rotation_y"],
        label: "Tilt Y",
        typed: "25.0",
        reset: 0.0,
    },
    Control {
        keys: &["perspective"],
        label: "Perspective",
        typed: "2.10",
        // Shown as 1, stored unset, as the card resets it (#1381).
        reset: 0.0,
    },
];

fn clip() -> klipp_timeline::Clip {
    fixture(
        &TransformParams {
            position_x: 12.0,
            position_y: -24.0,
            scale_x: 1.25,
            scale_y: 1.25,
            rotation: 8.0,
            rotation_x: 10.0,
            rotation_y: 15.0,
            perspective: 1.7,
            ..Default::default()
        }
        .to_builtin_effect(),
    )
}

fn run(clip: klipp_timeline::Clip) -> Run {
    let mut run = Run::new(clip);
    run.property = true;
    run
}

fn keys(control: Control, interaction: Interaction) -> Vec<CoverageKey> {
    control
        .keys
        .iter()
        .map(|key| CoverageKey::new(TYPE, key, Surface::VideoTab, VARIANT, interaction))
        .collect()
}

pub(super) fn require(manifest: &mut Manifest) {
    let entries = catalog();
    for control in CONTROLS {
        for key in control.keys {
            let entry = entries
                .iter()
                .find(|e| e.type_id == TYPE && e.parameter == *key)
                .unwrap();
            manifest.require_fixture(entry, Surface::VideoTab, VARIANT);
        }
    }
}

fn ready(run: &mut Run, label: &str) -> Result<(egui::Pos2, egui::Pos2), String> {
    run.frame(Vec::new(), egui::Modifiers::NONE);
    run.row(label)
        .ok_or_else(|| format!("Video row {label:?} is missing"))
}

fn begin_drag(run: &mut Run, label: &str) -> Result<egui::Pos2, String> {
    let (_, at) = ready(run, label)?;
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [6.0, 12.0, 18.0] {
        run.frame(
            vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))],
            egui::Modifiers::NONE,
        );
    }
    Ok(at + egui::vec2(18.0, 0.0))
}

#[derive(Clone, Copy, Debug)]
enum Edit {
    Type,
    Drag,
    Reset,
}

fn play(run: &mut Run, control: Control, edit: Edit) -> Result<(), String> {
    let (label, at) = ready(run, control.label)?;
    match edit {
        Edit::Type => {
            for _ in 0..2 {
                run.button(at, egui::PointerButton::Primary, true);
                run.button(at, egui::PointerButton::Primary, false);
            }
            run.frame(Vec::new(), egui::Modifiers::NONE);
            run.key(egui::Key::A, egui::Modifiers::COMMAND);
            run.frame(
                vec![egui::Event::Text(control.typed.into())],
                egui::Modifiers::NONE,
            );
            run.key(egui::Key::Enter, egui::Modifiers::NONE);
        }
        Edit::Drag => {
            let end = begin_drag(run, control.label)?;
            run.button(end, egui::PointerButton::Primary, false);
        }
        Edit::Reset => {
            run.button(label, egui::PointerButton::Primary, true);
            run.button(label, egui::PointerButton::Primary, false);
        }
    }
    run.frame(Vec::new(), egui::Modifiers::NONE);
    Ok(())
}

fn value(run: &Run, key: &str) -> f32 {
    let params = crate::inspector::video_tab::get_transform(run.clip());
    match key {
        "position_x" => params.position_x,
        "position_y" => params.position_y,
        "scale_x" => params.scale_x,
        "scale_y" => params.scale_y,
        "rotation" => params.rotation,
        "rotation_x" => params.rotation_x,
        "rotation_y" => params.rotation_y,
        "perspective" => params.perspective,
        _ => unreachable!(),
    }
}

fn edit_case(control: Control, edit: Edit) -> Result<(), String> {
    let clip = clip();
    let before = clip.picture_v2().unwrap();
    let start = clip.clone();
    let mut new = run(clip);
    play(&mut new, control, edit)?;
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", new.unapplied));
    }
    let after = new.clip().picture_v2().unwrap();
    if after == before {
        return Err("the control changed nothing".into());
    }
    let expected = match edit {
        Edit::Type => Some(control.typed.parse::<f32>().unwrap()),
        Edit::Reset => Some(control.reset),
        Edit::Drag => None,
    };
    // What it must store: the typed value, the reset value, exactly; a
    // drag to the right, a rise of every value it owns. Then the whole
    // picture: the start with those values written by path to its
    // placement, and nothing else.
    let was = run(start.clone());
    for key in control.keys {
        match expected {
            Some(expected) if value(&new, key) != expected => return Err(format!("{key} = {}, expected {expected}", value(&new, key))),
            None if value(&new, key) <= value(&was, key) => return Err(format!("a drag to the right left {key} at {} (from {})", value(&new, key), value(&was, key))),
            _ => {}
        }
    }
    // The document command: the start's Transform with those values set,
    // and nothing else.
    let mut params = crate::inspector::video_tab::get_transform(&start);
    for key in control.keys {
        let v = expected.unwrap_or_else(|| value(&new, key));
        match *key {
            "position_x" => params.position_x = v,
            "position_y" => params.position_y = v,
            "scale_x" => params.scale_x = v,
            "scale_y" => params.scale_y = v,
            "rotation" => params.rotation = v,
            "rotation_x" => params.rotation_x = v,
            "rotation_y" => params.rotation_y = v,
            "perspective" => params.perspective = v,
            _ => unreachable!(),
        }
    }
    let index = crate::inspector::video_tab::find_transform_index(&start).ok_or("the fixture has no Transform")?;
    let mut oracle = run(start.clone());
    let clip_id = oracle.clip;
    oracle.apply(klipp_core::AppCommand::UpdateBuiltinEffect { clip_id, index, effect: params.to_builtin_effect() });
    if !oracle.unapplied.is_empty() {
        return Err(format!("the oracle's command: unapplied {:?}", oracle.unapplied));
    }
    if oracle.clip().picture_v2().unwrap() != after {
        return Err("the edit left another picture than the Transform set by its document command does".into());
    }
    if new.state.active_history().undo_count() != 1 || new.state.any_drag_in_progress() {
        return Err(format!(
            "{} undo steps, drag open {}",
            new.state.active_history().undo_count(),
            new.state.any_drag_in_progress()
        ));
    }
    if !new.step(false) || new.clip().picture_v2().unwrap() != before {
        return Err("undo is not exact".into());
    }
    if !new.step(true) || new.clip().picture_v2().unwrap() != after {
        return Err("redo is not exact".into());
    }
    // The edit as the renderer draws it: changed, the CPU's as the GPU's.
    super::parity_pixels::clip_edit_drawn(&start, new.clip())
}

fn cancel_case(
    clip: klipp_timeline::Clip,
    control: Control,
) -> Result<(), String> {
    let mut run = run(clip);
    let before = run.clip().picture_v2().unwrap();
    let at = begin_drag(&mut run, control.label)?;
    if run.clip().picture_v2().unwrap() == before {
        return Err("cancel setup changed nothing".into());
    }
    run.key(egui::Key::Escape, egui::Modifiers::NONE);
    let end = at + egui::vec2(10.0, 0.0);
    run.frame(vec![egui::Event::PointerMoved(end)], egui::Modifiers::NONE);
    run.button(end, egui::PointerButton::Primary, false);
    if run.clip().picture_v2().unwrap() != before
        || run.state.active_history().undo_count() != 0
        || run.state.any_drag_in_progress()
        || !run.unapplied.is_empty()
    {
        return Err("Escape did not restore the whole picture without history".into());
    }
    Ok(())
}

pub(super) fn exercise(report: &mut Report) {
    for &control in CONTROLS {
        let name = |action: &str| format!("Video Transform / {} / {action}", control.label);
        report.case(
            &name("open"),
            keys(control, Interaction::OpenWithoutWrite),
            || {
                let mut run = run(clip());
                let before = run.clip().picture_v2().unwrap();
                let dirty = run.state.dirty;
                ready(&mut run, control.label)?;
                if run.clip().picture_v2().unwrap() != before
                    || run.state.dirty != dirty
                    || run.state.active_history().undo_count() != 0
                    || !run.unapplied.is_empty()
                {
                    return Err("opening writes state".into());
                }
                Ok(())
            },
        );
        for (edit, interaction) in [
            (Edit::Type, Interaction::TypeNumber),
            (Edit::Drag, Interaction::Drag),
            (Edit::Reset, Interaction::Reset),
        ] {
            let earned = keys(control, interaction)
                .into_iter()
                .chain(keys(control, Interaction::UndoRedo))
                .chain(keys(control, Interaction::Pixels));
            report.case(&name(&format!("{edit:?}")), earned, || {
                edit_case(control, edit)
            });
        }
        report.case(
            &name("cancel"),
            keys(control, Interaction::CancelDrag),
            || {
                cancel_case(clip(), control)
            },
        );
    }
}

/// The anchor Opacity's keyframe controls on the Video tab, against the
/// clip's opacity lane, earning the `anchor` keys
/// `parity_numeric` requires.
pub(super) fn anchor_opacity_keys(report: &mut Report) {
    use super::parity_effect_keys::{exercise_on, Action};
    let entry = catalog()
        .into_iter()
        .find(|e| e.type_id == "klipp.fx.opacity" && e.parameter == "value")
        .unwrap();
    for action in Action::ALL {
        let key = CoverageKey::new(entry.type_id, &entry.parameter, Surface::VideoTab, "anchor", action.interaction());
        report.case(&format!("Video Opacity / {action:?}"), [key], || {
            exercise_on(&entry, Surface::VideoTab, action)
        });
    }
}

#[test]
fn video_opacity_key_controls_keep_the_lane_and_undo_contracts() {
    let mut manifest = Manifest::default();
    super::parity_numeric::require(&mut manifest);
    let mut report = Report::default();
    anchor_opacity_keys(&mut report);
    report.family_result().unwrap();
    assert_eq!(report.passed.len(), 8);
    assert!(report
        .passed
        .iter()
        .all(|key| manifest.required.contains(key)));
}

#[test]
fn video_transform_controls_preserve_scalar_edit_and_history_contracts() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    exercise(&mut report);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty());
    assert!(report
        .passed
        .iter()
        .all(|key| manifest.required.contains(key)));
}

#[test]
fn video_transform_without_an_effect_cancels_clicks_and_undoes_the_first_drag() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let bare = klipp_timeline::Clip::media(
        klipp_core::MediaId::new(),
        tc(0.0),
        tc(2.0),
        tc(0.0),
        tc(2.0),
    );
    // Perspective appears only after a tilt; the tilted fixture above
    // covers its cancellation. These six rows exist on a bare clip.
    for &control in &CONTROLS[..6] {
        cancel_case(bare.clone(), control)
            .unwrap_or_else(|e| panic!("{}: {e}", control.label));
        let mut run = run(bare.clone());
        let before = run.clip().picture_v2().unwrap();
        let (_, at) = ready(&mut run, control.label).unwrap();
        run.button(at, egui::PointerButton::Primary, true);
        run.button(at, egui::PointerButton::Primary, false);
        run.frame(Vec::new(), egui::Modifiers::NONE);
        assert_eq!(
            run.clip().picture_v2().unwrap(),
            before,
            "{}: click",
            control.label
        );
        assert_eq!(run.state.active_history().undo_count(), 0);
        assert!(!run.state.any_drag_in_progress());
        assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);

        let mut edited = self::run(bare.clone());
        let end = begin_drag(&mut edited, control.label).unwrap();
        edited.button(end, egui::PointerButton::Primary, false);
        edited.frame(Vec::new(), egui::Modifiers::NONE);
        let after = edited.clip().picture_v2().unwrap();
        assert_ne!(
            after, before,
            "{}: committed drag",
            control.label
        );
        assert_eq!(edited.state.active_history().undo_count(), 1);
        assert!(!edited.state.any_drag_in_progress());
        assert!(edited.step(false));
        assert_eq!(
            edited.clip().picture_v2().unwrap(),
            before,
            "{}: undo",
            control.label
        );
        assert!(edited.step(true));
        assert_eq!(
            edited.clip().picture_v2().unwrap(),
            after,
            "{}: exact redo",
            control.label
        );
        assert!(edited.unapplied.is_empty(), "{:?}", edited.unapplied);
    }
}
