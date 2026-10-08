//! The inspector parity harness (M3 step 1c): one scripted interaction,
//! run through the old effects tab (switch off) and the generated stack
//! (switch on), each view's commands applied as the app applies them (the
//! path commands by the state, the index ones by the inspector edit host),
//! must leave the same clip picture (the full typed `picture_v2`), the
//! same undo depth, and the same analysis side commands.
//!
//! Each view is drawn headless and driven by real pointer and key events,
//! found from the frame's text as the other headless tests do. Both views
//! draw a number with `scrubby_param_row`, so one script drives both.

use super::parity_harness::{fixture, Run};
use klipp_core::AppCommand;
use klipp_effects::registry::presentation::Presentation;
use klipp_effects::registry::ParamDesc;
use klipp_effects::BuiltinEffect;

/// An interaction with a row's number.
#[derive(Clone, Copy, Debug)]
enum Script {
    /// S1: type a value inside the slider range and press Enter.
    Type,
    /// S2: press on the number, three moves (right, or left when the
    /// value sits in the upper half of its range), release.
    Drag { left: bool },
    /// S3: click the row's label, which resets it.
    Reset,
}

fn play(run: &mut Run, label: &str, script: Script, typed: &str) -> bool {
    let Some((label_at, at)) = run.row(label) else { return false };
    match script {
        Script::Type => {
            run.button(at, egui::PointerButton::Primary, true);
            run.button(at, egui::PointerButton::Primary, false);
            run.button(at, egui::PointerButton::Primary, true);
            run.button(at, egui::PointerButton::Primary, false);
            run.frame(Vec::new(), egui::Modifiers::NONE);
            run.key(egui::Key::A, egui::Modifiers::COMMAND);
            run.frame(vec![egui::Event::Text(typed.into())], egui::Modifiers::NONE);
            run.key(egui::Key::Enter, egui::Modifiers::NONE);
        }
        Script::Drag { left } => {
            let sign = if left { -1.0 } else { 1.0 };
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0, 18.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(sign * dx, 0.0))], egui::Modifiers::NONE);
            }
            run.frame(vec![egui::Event::PointerButton { pos: at + egui::vec2(sign * 18.0, 0.0), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
        }
        Script::Reset => {
            run.button(label_at, egui::PointerButton::Primary, true);
            run.button(label_at, egui::PointerButton::Primary, false);
        }
    }
    run.frame(Vec::new(), egui::Modifiers::NONE);
    true
}

/// Each effect type's first number row, with today's label and a value
/// inside its slider range to type.
fn first_number_row(t: &klipp_effects::registry::EffectType) -> Option<(&'static ParamDesc, Presentation, &'static str, String)> {
    use klipp_effects::registry::presentation::Widget;
    t.desc.params.iter().find_map(|p| {
        let pres = t.presentation(p);
        let numeric = matches!(p.kind, klipp_effects::registry::ParamKind::Float) && pres.widget == Widget::Scrub && pres.visible_if.is_none() && p.index.is_none();
        let (lo, hi) = p.soft?;
        numeric.then(|| (p, pres, pres.label.unwrap_or(p.label), format!("{:.*}", usize::from(pres.decimals), lo + (hi - lo) * 0.3)))
    })
}

/// The effect's stored value for `desc` on the run's clip.
fn value(run: &Run, desc: &ParamDesc) -> Option<f32> {
    value_of(run.clip().builtin_effects().first()?, desc)
}

fn value_of(effect: &BuiltinEffect, desc: &ParamDesc) -> Option<f32> {
    let entry = serde_json::to_value(effect).ok()?;
    crate::inspector::generated::row::number(&crate::inspector::generated::effect_value(&entry, desc))
}

/// `effect` with `desc` set to `v`, so a reset starts away from its
/// default.
fn with_value(effect: &BuiltinEffect, desc: &ParamDesc, v: f32) -> BuiltinEffect {
    let mut entry = serde_json::to_value(effect).unwrap();
    entry[desc.field] = serde_json::json!(v);
    serde_json::from_value(entry).unwrap()
}

/// The effects the generated view edits as clip properties, in the Video
/// tab, not as cards in the stack (M3 step 13): their row is driven there,
/// and the picture must still be the old card's.
const PROPERTIES: &[&str] = &["klipp.fx.opacity"];

/// The types whose reset differs from today by design: the value the old
/// view leaves and the generated row's, pinned.
/// - IC2: Sharpen's and Vibrance's amount reset to 0.5 in the old view
///   and to their catalog default, 0, in the generated row.
/// - IC4: Colour Fade's duration had no reset in the old view (the value
///   stays where the reset started it, 7.5125 s); the generated row
///   resets it to 1 s.
const RESET_DIFFERS: &[(&str, f32, f32)] = &[("klipp.fx.sharpen", 0.5, 0.0), ("klipp.fx.vibrance", 0.5, 0.0), ("klipp.fx.color_fade", 7.5125, 1.0)];

/// The undo depths that differ by design (old, new pinned):
/// - IC13: a drag of any row is one undo step. Colour Fade's duration was
///   a bare drag value, two steps for one drag; its picture still agrees.
/// - IC4: its reset, which the old view lacked, is one step.
const UNDO_DIFFERS: &[(&str, &str, usize, usize)] = &[("klipp.fx.color_fade", "Drag", 2, 1), ("klipp.fx.color_fade", "Reset", 0, 1)];

/// S1–S3 on every effect's first number row: the same picture, the same
/// undo depth and the same side commands, old view against new.
#[test]
fn number_rows_edit_the_same_through_both_views() {
    let mut differ = Vec::new();
    let mut compared = 0;
    for t in klipp_effects::registry::effect_types() {
        let Some((desc, pres, label, typed)) = first_number_row(t) else { continue };
        let typed_value: f32 = typed.parse().unwrap();
        let reset_to = crate::inspector::generated::row::reset_value(desc, &pres);
        let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() <= 0.5 * 10f32.powi(-i32::from(pres.decimals)) + 1e-6);
        let stored = value_of(&(t.sample)(), desc).unwrap_or(desc.default);
        let left = desc.soft.is_some_and(|(lo, hi)| stored > (lo + hi) / 2.0);
        for script in [Script::Type, Script::Drag { left }, Script::Reset] {
            let id = t.desc.id;
            let name = if matches!(script, Script::Drag { .. }) { "Drag".to_owned() } else { format!("{script:?}") };
            // A reset starts away from where it goes.
            let mut sample = (t.sample)();
            if matches!(script, Script::Reset) {
                let away = desc.soft.map_or(reset_to + 1.0, |(lo, hi)| if (reset_to - hi).abs() < (reset_to - lo).abs() { lo + (hi - lo) * 0.25 } else { lo + (hi - lo) * 0.75 });
                sample = with_value(&sample, desc, away);
            }
            let clip = fixture(&sample);
            let before = clip.picture_v2().unwrap().clone();
            let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip, true));
            new.property = PROPERTIES.contains(&id);
            let (o, n) = (play(&mut old, label, script, &typed), play(&mut new, label, script, &typed));
            if !(o && n) {
                differ.push(format!("{id} {script:?}: row {label:?} found old {o}, new {n}"));
                continue;
            }
            let (vo, vn) = (value(&old, desc), value(&new, desc));
            let (uo, un) = (old.state.active_history().undo_count(), new.state.active_history().undo_count());
            // What the script must have done, in each view.
            match script {
                Script::Type => {
                    if !near(vo, typed_value) || !near(vn, typed_value) {
                        differ.push(format!("{id} Type: typed {typed}, old {vo:?}, new {vn:?}"));
                    }
                }
                Script::Drag { .. } => {
                    if *new.clip().picture_v2().unwrap() == *before {
                        differ.push(format!("{id} Drag: the drag changed nothing"));
                    }
                    if un != 1 {
                        differ.push(format!("{id} Drag: {un} undo steps in the generated view, not one"));
                    }
                }
                Script::Reset => {
                    let pinned = RESET_DIFFERS.iter().find(|(r, _, _)| *r == id);
                    let (want_old, want_new) = pinned.map_or((reset_to, reset_to), |&(_, a, b)| (a, b));
                    if !near(vo, want_old) || !near(vn, want_new) {
                        differ.push(format!("{id} Reset: old {vo:?} (want {want_old}), new {vn:?} (want {want_new})"));
                    }
                }
            }
            let reset_differs = matches!(script, Script::Reset) && RESET_DIFFERS.iter().any(|(r, _, _)| *r == id);
            let (po, pn) = (old.clip().picture_v2().unwrap(), new.clip().picture_v2().unwrap());
            if po != pn && !reset_differs {
                differ.push(format!("{id} {script:?}: the pictures differ: old {:?}, new {:?}", old.clip().builtin_effects(), new.clip().builtin_effects()));
            }
            match UNDO_DIFFERS.iter().find(|(u, s, _, _)| *u == id && *s == name) {
                Some(&(_, _, want_old, want_new)) if (uo, un) != (want_old, want_new) => differ.push(format!("{id} {script:?}: undo depth old {uo}, new {un}, pinned {want_old}, {want_new}")),
                None if uo != un => differ.push(format!("{id} {script:?}: undo depth old {uo}, new {un}")),
                _ => {}
            }
            if old.side != new.side {
                differ.push(format!("{id} {script:?}: side commands old {:?}, new {:?}", old.side, new.side));
            }
            if !old.unapplied.is_empty() || !new.unapplied.is_empty() {
                differ.push(format!("{id} {script:?}: unapplied old {:?}, new {:?}", old.unapplied, new.unapplied));
            }
            compared += 1;
        }
    }
    assert!(differ.is_empty(), "{} differences in {compared} runs:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 100, "{compared} runs");
}


/// The name the new view shows for an effect's card.
fn card_name(t: &klipp_effects::registry::EffectType) -> &'static str {
    t.desc.name
}

/// S5 and S6 on every effect: turning it off, removing it, moving it below
/// another, and adding one, through each view's own control (the old
/// card's index commands, the new card's eye, ✕, ↓ and Add menu): the same
/// picture, or for an add, the same effects in the same order.
#[test]
fn card_edits_are_the_same_through_both_views() {
    let mut differ = Vec::new();
    let mut compared = 0;
    let other = BuiltinEffect::Sharpen { amount: 0.5 };
    for t in klipp_effects::registry::effect_types() {
        let effect = (t.sample)();
        let name = card_name(t);
        // A placed kind's card is the clip's own control: its eye, remove
        // and move are the effect list's, outside these.
        if crate::inspector::stack_view::is_placed_kind(t.tag) {
            continue;
        }
        let check = |what: &str, old: &Run, new: &Run, ids_equal: bool, differ: &mut Vec<String>| {
            let same = if ids_equal {
                old.clip().picture_v2().unwrap() == new.clip().picture_v2().unwrap()
            } else {
                old.clip().builtin_effects() == new.clip().builtin_effects() && old.clip().builtin_effects_enabled() == new.clip().builtin_effects_enabled()
            };
            if !same {
                differ.push(format!("{} {what}: old {:?}, new {:?}", t.desc.id, old.clip().builtin_effects(), new.clip().builtin_effects()));
            }
            if old.state.active_history().undo_count() != new.state.active_history().undo_count() {
                differ.push(format!("{} {what}: undo old {}, new {}", t.desc.id, old.state.active_history().undo_count(), new.state.active_history().undo_count()));
            }
            if !new.unapplied.is_empty() || !old.unapplied.is_empty() {
                differ.push(format!("{} {what}: unapplied old {:?}, new {:?}", t.desc.id, old.unapplied, new.unapplied));
            }
        };
        // S5: off.
        let clip = fixture(&effect);
        let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip.clone(), true));
        old.send(AppCommand::ToggleBuiltinEffectEnabled { clip_id: clip.id, index: 0 });
        if new.click_eye(name) { check("off", &old, &new, true, &mut differ) } else { differ.push(format!("{} off: no eye", t.desc.id)) }
        // S6: remove.
        let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip.clone(), true));
        old.send(AppCommand::RemoveBuiltinEffect { clip_id: clip.id, index: 0 });
        if new.click_text(name, "✕") { check("remove", &old, &new, true, &mut differ) } else { differ.push(format!("{} remove: no ✕", t.desc.id)) }
        // S6: move below another.
        let mut two = clip.clone();
        two.push_effect(other.clone());
        let (mut old, mut new) = (Run::new(two.clone(), false), Run::new(two, true));
        old.send(AppCommand::ReorderBuiltinEffect { clip_id: clip.id, from_index: 0, to_index: 1 });
        if new.click_text(name, "↓") { check("move", &old, &new, true, &mut differ) } else { differ.push(format!("{} move: no ↓", t.desc.id)) }
        // S6: add, onto a clip whose effect has another name, so the menu
        // item is the only one.
        let base = fixture(if t.tag == "sharpen" { &BuiltinEffect::Invert { mix: 1.0 } } else { &other });
        let (mut old, mut new) = (Run::new(base.clone(), false), Run::new(base.clone(), true));
        old.send(AppCommand::AddBuiltinEffect { clip_id: base.id, effect: effect.clone() });
        if new.click_text("", "+ Add effect") && new.click_menu_item(name) { check("add", &old, &new, false, &mut differ) } else { differ.push(format!("{} add: not in the menu", t.desc.id)) }
        compared += 4;
    }
    assert!(differ.is_empty(), "{} differences in {compared} runs:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 150, "{compared} runs");
}

/// S7 (M3 step 14): every built-in preset, picked from the card's Preset
/// menu in each view (the old card's by its index, the generated card's by
/// its path; a clip property's, Opacity's, from its row in the Video tab):
/// the same picture, the same undo depth.
#[test]
fn a_preset_picked_from_the_card_is_the_same_through_both_views() {
    let presets = crate::effect_presets::PresetManager::with_user_dir(std::path::PathBuf::from("/nonexistent/klipp-presets"));
    let mut differ = Vec::new();
    let mut compared = 0;
    for t in klipp_effects::registry::effect_types() {
        let builtins: Vec<_> = presets.presets_for_type(t.desc.id).into_iter().filter(|p| p.is_builtin).collect();
        if builtins.is_empty() {
            continue;
        }
        for preset in builtins {
            let clip = fixture(&(t.sample)());
            let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip, true));
            new.property = PROPERTIES.contains(&t.desc.id);
            let (o, n) = (
                old.click_text("", "Select preset...") && old.click_menu_item(&preset.name),
                new.click_text("", "Select preset...") && new.click_menu_item(&preset.name),
            );
            if !(o && n) {
                differ.push(format!("{} {:?}: picked old {o}, new {n}", t.desc.id, preset.name));
                continue;
            }
            if old.clip().picture_v2().unwrap() != new.clip().picture_v2().unwrap() {
                differ.push(format!("{} {:?}: the pictures differ: old {:?}, new {:?}", t.desc.id, preset.name, old.clip().builtin_effects(), new.clip().builtin_effects()));
            }
            let (uo, un) = (old.state.active_history().undo_count(), new.state.active_history().undo_count());
            if (uo, un) != (1, 1) {
                differ.push(format!("{} {:?}: undo depth old {uo}, new {un}", t.desc.id, preset.name));
            }
            if !old.unapplied.is_empty() || !new.unapplied.is_empty() {
                differ.push(format!("{} {:?}: unapplied old {:?}, new {:?}", t.desc.id, preset.name, old.unapplied, new.unapplied));
            }
            compared += 1;
        }
    }
    assert!(differ.is_empty(), "{} differences in {compared} presets:\n{}", differ.len(), differ.join("\n"));
    assert_eq!(compared, 135, "every built-in preset compared");
}

/// M3 step 14: every built-in entry of the effects browser is a catalog
/// type, and adding it behind the switch (by path, after the clip's stack)
/// leaves the clip the same effects, in the same order and with the same
/// values (the entry's own, not the catalog's sample), as adding it to the
/// effect list does.
#[test]
fn a_browser_add_is_the_same_by_path() {
    let registry = crate::browser::effect_registry::EffectRegistry::new();
    let mut differ = Vec::new();
    let mut compared = 0;
    for entry in &registry.effects {
        let crate::browser::effect_registry::EffectSource::Builtin(effect) = &entry.source else { continue };
        if entry.type_id().is_none() {
            differ.push(format!("{}: no catalog type", entry.name));
            continue;
        }
        let clip = fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 });
        let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip, true));
        let id = old.clip;
        old.apply(AppCommand::AddBuiltinEffect { clip_id: id, effect: effect.clone() });
        let Some(by_path) = crate::inspector::stack_view::add_effect_command(&new.state, new.clip(), effect) else {
            differ.push(format!("{}: no add by path", entry.name));
            continue;
        };
        new.apply(by_path);
        let list = |r: &Run| serde_json::to_value(r.clip().builtin_effects()).unwrap();
        if list(&old) != list(&new) {
            differ.push(format!("{}: old {}, new {}", entry.name, list(&old), list(&new)));
        }
        if !old.unapplied.is_empty() || !new.unapplied.is_empty() {
            differ.push(format!("{}: unapplied old {:?}, new {:?}", entry.name, old.unapplied, new.unapplied));
        }
        compared += 1;
    }
    assert!(differ.is_empty(), "{} differences in {compared} entries:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 40, "only {compared} entries compared");
}


/// Start a real drag on the header's Mix slider. Its numeric value is
/// drawn between the rail and the Mix label; use that observed row and
/// egui's configured rail width, not an absolute screen coordinate.
fn start_header_mix_drag(run: &mut Run) -> egui::Pos2 {
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    let label = drawn
        .iter()
        .find(|(text, _)| text == "Mix")
        .map(|(_, at)| *at)
        .expect("Mix label");
    let number = drawn
        .iter()
        .filter(|(text, at)| {
            (at.y - label.y).abs() < 6.0 && at.x < label.x && text.parse::<f32>().is_ok()
        })
        .max_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .map(|(_, at)| *at)
        .unwrap_or_else(|| panic!("Mix value: {drawn:?}"));
    let at = number - egui::vec2(run.ctx.style().spacing.slider_width * 0.6, 0.0);
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [6.0, 12.0, 18.0] {
        run.frame(
            vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))],
            egui::Modifiers::NONE,
        );
    }
    at + egui::vec2(18.0, 0.0)
}

#[test]
fn a_header_mix_drag_is_one_exact_undo_step() {
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }), true);
    let before = run.clip().picture_v2().unwrap();
    let at = start_header_mix_drag(&mut run);
    run.button(at, egui::PointerButton::Primary, false);
    let after = run.clip().picture_v2().unwrap();
    assert_ne!(
        after, before,
        "the real Mix control must change the picture"
    );
    assert_eq!(
        run.state.active_history().undo_count(),
        1,
        "one drag, not one step per frame"
    );
    assert!(!run.state.any_drag_in_progress());
    assert!(run.step(false));
    assert_eq!(run.clip().picture_v2().unwrap(), before);
    assert!(run.step(true));
    assert_eq!(run.clip().picture_v2().unwrap(), after);
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
}

#[test]
fn escape_abandons_a_header_mix_drag_even_if_the_pointer_keeps_moving() {
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }), true);
    let before = run.clip().picture_v2().unwrap();
    let at = start_header_mix_drag(&mut run);
    assert_ne!(
        run.clip().picture_v2().unwrap(),
        before,
        "the real Mix control must change the picture"
    );
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
    assert_eq!(
        run.clip().picture_v2().unwrap(),
        before,
        "Escape restores the full picture"
    );
    assert_eq!(run.state.active_history().undo_count(), 0);
    assert!(!run.state.any_drag_in_progress());
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
}

/// Escape is an intended improvement over the old card: its drag commits,
/// while the generated row must restore the whole picture without history.
#[test]
fn escape_cancels_a_generated_number_drag_while_legacy_keeps_its_edit() {
    for (effect, label) in [
        (BuiltinEffect::GaussianBlur { radius: 3.0 }, "Radius"),
        (BuiltinEffect::TemporalDenoise { strength: 0.5, threshold: 0.02 }, "Strength"),
    ] {
        for generated in [false, true] {
            let mut run = Run::new(fixture(&effect), generated);
            let before = run.clip().picture_v2().unwrap();
            run.frame(Vec::new(), egui::Modifiers::NONE);
            let (_, at) = run.row(label).expect("numeric row");
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0, 18.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            let during = run.clip().picture_v2().unwrap();
            assert_ne!(during, before, "the real control must edit before Escape");
            run.key(egui::Key::Escape, egui::Modifiers::NONE);
            run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(28.0, 0.0))], egui::Modifiers::NONE);
            run.button(at + egui::vec2(28.0, 0.0), egui::PointerButton::Primary, false);
            let expected = if generated { &before } else { &during };
            assert_eq!(&run.clip().picture_v2().unwrap(), expected, "Escape in generated={generated}");
            assert_eq!(run.state.active_history().undo_count(), usize::from(!generated));
            assert!(!run.state.any_drag_in_progress());
            assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
            if !generated {
                assert!(run.step(false));
                assert_eq!(run.clip().picture_v2().unwrap(), before);
                assert!(run.step(true));
                assert_eq!(run.clip().picture_v2().unwrap(), during);
            }
        }
    }
}

#[test]
fn perspective_label_reset_restores_unset_below_its_soft_range() {
    let mut transform = crate::inspector::video_tab::TransformParams {
        perspective: 1.7, rotation_y: 15.0, ..Default::default()
    };
    let clip = fixture(&transform.to_builtin_effect());
    let mut run = Run::new(clip.clone(), true);
    let mut oracle = Run::new(clip, false);
    let before = run.clip().picture_v2().unwrap();
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let (label, _) = run.row("Perspective").expect("perspective row");
    run.button(label, egui::PointerButton::Primary, true);
    run.button(label, egui::PointerButton::Primary, false);
    transform.perspective = 0.0;
    oracle.apply(AppCommand::UpdateBuiltinEffect {
        clip_id: oracle.clip, index: 0, effect: transform.to_builtin_effect(),
    });
    let after = run.clip().picture_v2().unwrap();
    assert_eq!(after, oracle.clip().picture_v2().unwrap());
    assert_ne!(after, before);
    run.frame(Vec::new(), egui::Modifiers::NONE);
    assert_eq!(run.clip().picture_v2().unwrap(), after, "drawing preserves unset");
    assert_eq!(run.state.active_history().undo_count(), 1);
    assert!(!run.state.any_drag_in_progress());
    assert!(run.step(false));
    assert_eq!(run.clip().picture_v2().unwrap(), before);
    assert!(run.step(true));
    assert_eq!(run.clip().picture_v2().unwrap(), after);
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
}

fn plain_slider_cases() -> Vec<(BuiltinEffect, BuiltinEffect, &'static str)> {
    [("noise_generator", "seed", 1000), ("radial_blur", "samples", 24)]
        .into_iter().map(|(tag, key, value)| {
            let ty = klipp_effects::registry::effect_type(tag).unwrap();
            let desc = ty.desc.params.iter().find(|p| p.key == key).unwrap();
            let mut json = serde_json::to_value((ty.sample)()).unwrap();
            json[desc.field] = serde_json::json!(value);
            let changed = serde_json::from_value(json.clone()).unwrap();
            json[desc.field] = serde_json::json!(desc.default as u32);
            (changed, serde_json::from_value(json).unwrap(), desc.label)
        }).collect()
}

/// IC15: plain sliders gain the same label reset as other number rows.
#[test]
fn plain_slider_labels_reset_with_exact_undo_and_redo() {
    for (effect, reset, label) in plain_slider_cases() {
        let clip = fixture(&effect);
        let mut run = Run::new(clip.clone(), true);
        let mut oracle = Run::new(clip, false);
        let before = run.clip().picture_v2().unwrap();
        run.frame(Vec::new(), egui::Modifiers::NONE);
        let (at, _) = run.row(label).expect("slider row");
        run.button(at, egui::PointerButton::Primary, true);
        run.button(at, egui::PointerButton::Primary, false);
        oracle.apply(AppCommand::UpdateBuiltinEffect { clip_id: oracle.clip, index: 0, effect: reset });
        let after = run.clip().picture_v2().unwrap();
        assert_ne!(after, before, "{label}");
        assert_eq!(after, oracle.clip().picture_v2().unwrap(), "{label}");
        assert_eq!(run.state.active_history().undo_count(), 1);
        assert!(run.step(false));
        assert_eq!(run.clip().picture_v2().unwrap(), before);
        assert!(run.step(true));
        assert_eq!(run.clip().picture_v2().unwrap(), after);
        assert!(!run.state.any_drag_in_progress());
        assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    }
}

/// IC14 covers both the slider rail and its editable number.
#[test]
fn plain_slider_escape_cancels_rail_and_number_drags() {
    for (effect, _, label) in plain_slider_cases() {
        for rail in [false, true] {
            let mut run = Run::new(fixture(&effect), true);
            let before = run.clip().picture_v2().unwrap();
            run.frame(Vec::new(), egui::Modifiers::NONE);
            let (_, number) = run.row(label).expect("slider row");
            let at = if rail {
                number - egui::vec2(run.ctx.style().spacing.slider_width * 0.6, 0.0)
            } else { number };
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0, 18.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            assert_ne!(run.clip().picture_v2().unwrap(), before, "{label}, rail={rail}");
            run.key(egui::Key::Escape, egui::Modifiers::NONE);
            run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(28.0, 0.0))], egui::Modifiers::NONE);
            run.button(at + egui::vec2(28.0, 0.0), egui::PointerButton::Primary, false);
            assert_eq!(run.clip().picture_v2().unwrap(), before, "{label}, rail={rail}");
            assert_eq!(run.state.active_history().undo_count(), 0);
            assert!(!run.state.any_drag_in_progress());
            assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
        }
    }
}

/// Escape on the Video tab's Opacity drag takes the gesture back, as it
/// does on a number row (#1364, IC14): the picture from before the drag,
/// no undo step, and the pointer moving on changes nothing.
#[test]
fn escape_cancels_the_video_tab_opacity_drag() {
    let mut run = Run::new(fixture(&BuiltinEffect::Opacity { value: 0.8 }), true);
    run.property = true;
    let before = run.clip().picture_v2().unwrap();
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let (_, at) = run.row("Opacity").expect("the Video tab's Opacity row");
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [-6.0, -12.0, -18.0] {
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
    }
    let during = run.clip().picture_v2().unwrap();
    assert_ne!(during, before, "the real control must edit before Escape");
    run.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert_eq!(run.clip().picture_v2().unwrap(), before, "Escape restores the picture from before the drag");
    for dx in [-28.0, -40.0] {
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
    }
    run.button(at + egui::vec2(-40.0, 0.0), egui::PointerButton::Primary, false);
    assert_eq!(run.clip().picture_v2().unwrap(), before, "moving on after Escape changes nothing");
    assert_eq!(run.state.active_history().undo_count(), 0, "a cancelled drag leaves no undo step");
    assert!(!run.state.any_drag_in_progress());
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
}

fn soft_edge_clip(keyed: bool) -> klipp_timeline::Clip {
    let ty = klipp_effects::registry::effect_type("chroma_key").unwrap();
    let desc = ty.desc.params.iter().find(|p| p.key == "spill").unwrap();
    let mut clip = fixture(&with_value(&(ty.sample)(), desc, 0.2));
    if keyed {
        clip.edit_effect_keyframes(|tracks| {
            tracks.insert("0:spill".into(), vec![klipp_core::keyframe::upserted(&[], 0.0, 0.4)]);
        });
    }
    clip
}

#[test]
fn soft_edge_alias_edits_match_the_old_control() {
    for script in [Script::Type, Script::Drag { left: false }, Script::Reset] {
        let clip = soft_edge_clip(false);
        let before = clip.picture_v2().unwrap();
        let mut old = Run::new(clip.clone(), false);
        let mut new = Run::new(clip, true);
        assert!(play(&mut old, "Soft Edge", script, "0.700"));
        assert!(play(&mut new, "Soft Edge", script, "0.700"));
        let after = new.clip().picture_v2().unwrap();
        assert_ne!(after, before, "{script:?}");
        assert_eq!(after, old.clip().picture_v2().unwrap(), "{script:?}");
        assert_eq!(new.state.active_history().undo_count(), 1);
        assert!(!new.state.any_drag_in_progress());
        assert!(new.step(false));
        assert_eq!(new.clip().picture_v2().unwrap(), before);
        assert!(new.step(true));
        assert_eq!(new.clip().picture_v2().unwrap(), after);
        assert!(new.unapplied.is_empty(), "{:?}", new.unapplied);
        assert!(old.unapplied.is_empty(), "{:?}", old.unapplied);
    }
}

#[test]
fn soft_edge_alias_reads_and_edits_its_existing_animation() {
    let clip = soft_edge_clip(true);
    let before = clip.picture_v2().unwrap();
    let mut new = Run::new(clip.clone(), true);
    let mut oracle = clip;
    new.frame(Vec::new(), egui::Modifiers::NONE);
    let (_, number) = new.row("Soft Edge").unwrap();
    let drawn = new.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(drawn.iter().any(|(s, p)| p.distance(number) < 1.0 && s.parse::<f32>() == Ok(0.4)), "the existing key is shown");
    assert!(play(&mut new, "Soft Edge", Script::Type, "0.700"));
    // Use the clip's existing legacy lane API as the reference: the
    // shared row driver does not dispatch old keyframe commands.
    let effect = oracle.effect_id_at(0).unwrap();
    oracle.edit_effect_keyframes_of(effect, "spill", |keys| klipp_core::keyframe::upsert_keyframe(keys, 0.0, 0.7));
    let after = new.clip().picture_v2().unwrap();
    assert_ne!(after, before);
    assert_eq!(after, oracle.picture_v2().unwrap());
    assert_eq!(new.state.active_history().undo_count(), 1);
    assert!(new.step(false));
    assert_eq!(new.clip().picture_v2().unwrap(), before);
    assert!(new.step(true));
    assert_eq!(new.clip().picture_v2().unwrap(), after);
    assert!(new.unapplied.is_empty(), "{:?}", new.unapplied);
}

#[test]
fn soft_edge_alias_drag_cancels_static_and_animated_edits() {
    for keyed in [false, true] {
        let mut run = Run::new(soft_edge_clip(keyed), true);
        let before = run.clip().picture_v2().unwrap();
        run.frame(Vec::new(), egui::Modifiers::NONE);
        let (_, at) = run.row("Soft Edge").unwrap();
        run.button(at, egui::PointerButton::Primary, true);
        for dx in [6.0, 12.0, 18.0] {
            run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
        }
        assert_ne!(run.clip().picture_v2().unwrap(), before, "keyed={keyed}");
        run.key(egui::Key::Escape, egui::Modifiers::NONE);
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(28.0, 0.0))], egui::Modifiers::NONE);
        run.button(at + egui::vec2(28.0, 0.0), egui::PointerButton::Primary, false);
        assert_eq!(run.clip().picture_v2().unwrap(), before, "keyed={keyed}");
        assert_eq!(run.state.active_history().undo_count(), 0);
        assert!(!run.state.any_drag_in_progress());
        assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    }
}

/// A clip with no Opacity effect yet, its Video tab in either view (#1364):
/// the old view adds the effect as the drag starts, the generated view
/// drags the clip's own opacity. Escape takes the whole drag back, the add
/// with it; a press and release changes nothing; a drag commits, and undo
/// takes it all back.
#[test]
fn a_clip_without_opacity_cancels_commits_and_undoes_its_first_drag() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let bare = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    for generated in [false, true] {
        let start = |run: &mut Run| {
            run.property = true;
            run.frame(Vec::new(), egui::Modifiers::NONE);
            run.row("Opacity").expect("the Video tab's Opacity row").1
        };
        // Escape mid-drag.
        let mut run = Run::new(bare.clone(), generated);
        let before = run.clip().picture_v2().unwrap();
        let at = start(&mut run);
        run.button(at, egui::PointerButton::Primary, true);
        for dx in [-6.0, -12.0, -18.0] {
            run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
        }
        assert_ne!(run.clip().picture_v2().unwrap(), before, "generated={generated}: the drag edits");
        run.key(egui::Key::Escape, egui::Modifiers::NONE);
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(-30.0, 0.0))], egui::Modifiers::NONE);
        run.button(at + egui::vec2(-30.0, 0.0), egui::PointerButton::Primary, false);
        assert_eq!(run.clip().picture_v2().unwrap(), before, "generated={generated}: Escape leaves no effect behind");
        assert_eq!(run.state.active_history().undo_count(), 0, "generated={generated}");
        assert!(!run.state.any_drag_in_progress());
        assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
        // A press and a release, nothing moved.
        let mut run = Run::new(bare.clone(), generated);
        let at = start(&mut run);
        run.button(at, egui::PointerButton::Primary, true);
        run.button(at, egui::PointerButton::Primary, false);
        run.frame(Vec::new(), egui::Modifiers::NONE);
        assert_eq!(run.clip().picture_v2().unwrap(), before, "generated={generated}: a click adds nothing");
        assert_eq!(run.state.active_history().undo_count(), 0, "generated={generated}");
        // A drag commits, and undo takes all of it back.
        let mut run = Run::new(bare.clone(), generated);
        let at = start(&mut run);
        run.button(at, egui::PointerButton::Primary, true);
        for dx in [-6.0, -12.0, -18.0] {
            run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
        }
        run.button(at + egui::vec2(-18.0, 0.0), egui::PointerButton::Primary, false);
        run.frame(Vec::new(), egui::Modifiers::NONE);
        let after = run.clip().picture_v2().unwrap();
        assert_ne!(after, before, "generated={generated}: the drag is kept");
        assert!(!run.state.any_drag_in_progress());
        // One step in either view, the add and its value together (IC13).
        assert_eq!(run.state.active_history().undo_count(), 1, "generated={generated}");
        assert!(run.step(false));
        assert_eq!(run.clip().picture_v2().unwrap(), before, "generated={generated}: undo takes it all back");
        assert!(run.step(true));
        let redone = run.clip().picture_v2().unwrap();
        if generated {
            assert_eq!(redone, after, "generated={generated}: redo puts it back exactly");
        } else {
            // The old view's redo gives the re-added Opacity node a new id
            // (#1373); everything else comes back exactly.
            let without_opacity_id = |p: &klipp_timeline::LiftedPicture| {
                let text = serde_json::to_string(&p.picture).unwrap();
                let id = p.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == "klipp.legacy.opacity").expect("an Opacity node").id.to_string();
                text.replace(&id, "OPACITY")
            };
            assert_eq!(without_opacity_id(&redone), without_opacity_id(&after), "redo puts it back, but for the node id (#1373)");
        }
        assert!(run.step(false));
        assert_eq!(run.clip().picture_v2().unwrap(), before, "generated={generated}: undo takes it all back");
        assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    }
}

/// A first drag that adds Opacity and ends unseen by its row (released
/// while the row isn't drawn) leaves no mark of its add. Later the user
/// removes that effect and adds their own; a drag on it cancelled by
/// Escape keeps it, though an add is the last step in the history (#1364).
#[test]
fn an_unseen_end_of_an_adding_drag_never_lets_escape_remove_a_kept_opacity() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let bare = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut run = Run::new(bare, false);
    run.property = true;
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let (_, at) = run.row("Opacity").expect("the Video tab's Opacity row");
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [-6.0, -12.0] {
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
    }
    // Released while the row isn't drawn: it never sees the drag stop.
    run.property = false;
    run.button(at + egui::vec2(-12.0, 0.0), egui::PointerButton::Primary, false);
    run.frame(Vec::new(), egui::Modifiers::NONE);
    // The user removes that effect and adds their own Opacity: an add is
    // now the history's last step.
    let index = crate::inspector::video_tab::find_opacity_index(run.clip()).expect("the first drag added Opacity");
    run.apply(AppCommand::RemoveBuiltinEffect { clip_id: run.clip, index });
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Opacity { value: 0.5 } });
    run.property = true;
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let kept = run.clip().picture_v2().unwrap();
    // A drag on it, cancelled by Escape.
    let (_, at) = run.row("Opacity").expect("the Opacity row");
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [-6.0, -12.0, -18.0] {
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
    }
    run.key(egui::Key::Escape, egui::Modifiers::NONE);
    run.button(at + egui::vec2(-18.0, 0.0), egui::PointerButton::Primary, false);
    run.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(crate::inspector::video_tab::find_opacity_index(run.clip()).is_some(), "Escape keeps the user's own Opacity");
    assert_eq!(run.clip().picture_v2().unwrap(), kept, "and the picture as it was before the drag");
    assert!(!run.state.any_drag_in_progress());
}

/// The node id an effect of `kind` (its picture type) has in the clip.
fn node_id_of(run: &Run, type_id: &str) -> Option<uuid::Uuid> {
    run.clip().picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == type_id).map(|n| n.id)
}

/// What the project saves of the clip: its fields and its stored picture.
fn saved(run: &Run) -> String {
    format!("{}\n{}", serde_json::to_string(run.clip()).unwrap(), serde_json::to_string(&run.clip().picture_v2().unwrap().picture).unwrap())
}

/// #1373: an effect beside one added and taken back keeps its node id, so
/// a path held to it (a selection, a key) still names it; and the added
/// effect comes back under the id it was added with. Opacity's add is
/// recorded as the effect list, which an undo used to put back under new
/// ids for every effect of the clip.
#[test]
fn undo_and_redo_of_an_add_keep_every_effects_node_id() {
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }), false);
    let blur = node_id_of(&run, "klipp.legacy.effect").expect("the Blur's node");
    let held = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: run.clip.0 }, vec![blur]);
    let resolves = |run: &Run| run.clip().picture_v2().unwrap().picture.graph.nodes.iter().any(|n| Some(&n.id) == held.nodes.last());
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Opacity { value: 0.5 } });
    let opacity = node_id_of(&run, "klipp.legacy.opacity").expect("Opacity added");
    let (picture, bytes) = (run.clip().picture_v2().unwrap(), saved(&run));
    assert!(run.step(false), "undo the add");
    assert_eq!(node_id_of(&run, "klipp.legacy.effect"), Some(blur), "the Blur keeps its id through the undo");
    assert!(resolves(&run), "the path held to the Blur still names it after the undo");
    assert_eq!(node_id_of(&run, "klipp.legacy.opacity"), None);
    assert!(run.step(true), "redo it");
    assert_eq!(node_id_of(&run, "klipp.legacy.effect"), Some(blur), "the Blur keeps its id through the redo");
    assert!(resolves(&run), "and its path still names it");
    assert_eq!(node_id_of(&run, "klipp.legacy.opacity"), Some(opacity), "the Opacity comes back under the id it was added with");
    assert_eq!(run.clip().picture_v2().unwrap(), picture, "the same picture");
    assert_eq!(saved(&run), bytes, "the same saved bytes");
}

/// #1373: taking back a later effect's add leaves the ids of those added
/// before it alone, whichever way each add was recorded.
#[test]
fn undoing_a_later_add_leaves_earlier_effects_ids_alone() {
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }), false);
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Opacity { value: 0.5 } });
    let opacity = node_id_of(&run, "klipp.legacy.opacity").expect("Opacity added");
    let blur = node_id_of(&run, "klipp.legacy.effect").expect("the Blur");
    // A later add of a plain effect, recorded as a picture edit.
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Vignette { amount: 0.4, size: 0.7, softness: 0.4, roundness: 0.0 } });
    assert!(run.step(false), "undo the later add");
    assert_eq!(node_id_of(&run, "klipp.legacy.opacity"), Some(opacity), "Opacity keeps its id");
    assert!(run.clip().picture_v2().unwrap().picture.graph.nodes.iter().any(|n| n.id == blur), "the Blur keeps its id");
    assert!(run.step(true), "redo it");
    assert_eq!(node_id_of(&run, "klipp.legacy.opacity"), Some(opacity));
    assert!(run.clip().picture_v2().unwrap().picture.graph.nodes.iter().any(|n| n.id == blur));
}

