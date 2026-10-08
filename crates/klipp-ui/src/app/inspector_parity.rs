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
