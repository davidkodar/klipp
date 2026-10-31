//! The generated inspector's effect rows, driven headless by real pointer
//! and key events found from the frame's text, each command applied as the
//! app applies it (the path commands by the state, the index ones by the
//! inspector edit host): what each edit stores, its undo, and what Escape
//! takes back.

use super::parity_harness::{fixture, Run};
use klipp_core::AppCommand;
use klipp_effects::registry::ParamDesc;
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip;

/// An interaction with a row's number.
#[derive(Clone, Copy, Debug)]
enum Script {
    /// S1: type a value inside the slider range and press Enter.
    Type,
}

fn play(run: &mut Run, label: &str, script: Script, typed: &str) -> bool {
    let Some((_, at)) = run.row(label) else { return false };
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
    }
    run.frame(Vec::new(), egui::Modifiers::NONE);
    true
}

/// `effect` with `desc` set to `v`, so a reset starts away from its
/// default.
fn with_value(effect: &BuiltinEffect, desc: &ParamDesc, v: f32) -> BuiltinEffect {
    let mut entry = serde_json::to_value(effect).unwrap();
    entry[desc.field] = serde_json::json!(v);
    serde_json::from_value(entry).unwrap()
}

/// M3 steps 14 and 19: every built-in entry of the effects browser is a
/// catalog type, and every built-in effect and every look it offers, added
/// through the browser's own producer (`effect_commands_in`, as its double
/// click and a drop send it), leaves the clip those effects after its own,
/// in order and with the entry's own values (not the catalog's sample),
/// by path, as one undo step, undone exactly.
#[test]
fn a_browser_add_puts_its_effects_after_the_stack() {
    use crate::browser::effect_registry::EffectSource;
    let registry = crate::browser::effect_registry::EffectRegistry::new();
    let mut differ = Vec::new();
    let (mut compared, mut looks) = (0, 0);
    for entry in &registry.effects {
        if !matches!(entry.source, EffectSource::Builtin(_) | EffectSource::Look(_)) {
            continue;
        }
        if matches!(entry.source, EffectSource::Builtin(_)) && entry.type_id().is_none() {
            differ.push(format!("{}: no catalog type", entry.name));
            continue;
        }
        let base = BuiltinEffect::GaussianBlur { radius: 3.0 };
        let mut run = Run::new(fixture(&base));
        let targets = crate::effect_targets::Targets { clips: vec![run.clip], skipped: 0 };
        let before = run.clip().picture_v2().unwrap();
        match crate::effect_targets::all_or_none(&targets, |id| crate::browser::effects_tab::effect_commands_in(&run.state, &entry.source, id)) {
            Ok(cmds) => cmds.into_iter().for_each(|cmd| run.apply(cmd)),
            Err(why) => differ.push(format!("{}: refused ({why})", entry.name)),
        }
        let want: Vec<BuiltinEffect> = std::iter::once(base).chain(crate::browser::effects_tab::starting_effects(&run.state, &entry.source).unwrap_or_default()).collect();
        let list = |effects: &[BuiltinEffect]| serde_json::to_value(effects).unwrap();
        if list(&test_clip::effects(run.clip())) != list(&want) {
            differ.push(format!("{}: {} where {} is expected", entry.name, list(&test_clip::effects(run.clip())), list(&want)));
        }
        if !run.unapplied.is_empty() {
            differ.push(format!("{}: unapplied {:?}", entry.name, run.unapplied));
        }
        if run.state.active_history().undo_count() != 1 {
            differ.push(format!("{}: {} undo steps", entry.name, run.state.active_history().undo_count()));
        } else if !run.step(false) || run.clip().picture_v2().unwrap() != before {
            differ.push(format!("{}: undo isn't exact", entry.name));
        }
        compared += 1;
        looks += usize::from(matches!(entry.source, EffectSource::Look(_)));
    }
    assert!(differ.is_empty(), "browser adds differ:\n{}", differ.join("\n"));
    assert!(compared > 40, "only {compared} entries compared");
    assert!(looks > 0, "no look compared");
}

/// A browser add the clip can't take is refused with its reason, never
/// sent as the old index add instead (M3 step 19).
#[test]
fn a_browser_add_the_clip_cant_take_is_refused() {
    let run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }));
    let source = crate::browser::effect_registry::EffectSource::Builtin(BuiltinEffect::Sharpen { amount: 0.5 });
    let refused = crate::browser::effects_tab::effect_commands_in(&run.state, &source, klipp_core::ClipId::new());
    assert_eq!(refused.unwrap_err(), "the clip isn't in the sequence");
}

/// A look put on two selected clips, as a drop or the
/// browser's double click sends it (`effect_targets::all_or_none` over
/// `effect_commands_in`): its whole chain by path on both, one undo step,
/// undone exactly on both.
#[test]
fn a_look_on_two_clips_is_one_step_by_path() {
    use crate::browser::effect_registry::EffectSource;
    let registry = crate::browser::effect_registry::EffectRegistry::new();
    let (name, chain) = registry.effects.iter().find_map(|e| match &e.source {
        EffectSource::Look(chain) if chain.len() > 1 && chain.iter().all(|fx| !crate::inspector::stack_view::is_placed_kind(serde_json::to_value(fx).unwrap()["type"].as_str().unwrap())) => Some((e.name.clone(), e.source.clone())),
        _ => None,
    }).expect("a look of several effects");
    let EffectSource::Look(effects) = &chain else { unreachable!() };
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }));
    let mut second = run.clip().clone();
    second.id = klipp_core::ClipId::new();
    let other = second.id;
    let v1 = run.state.sequences[0].tracks.iter().position(|t| t.clips.iter().any(|c| c.id == run.clip)).unwrap();
    run.state.sequences[0].tracks[v1].clips.push(second);
    let ids = [run.clip, other];
    let targets = crate::effect_targets::Targets { clips: ids.to_vec(), skipped: 0 };
    let pictures = |r: &Run| ids.map(|id| r.state.sequences[0].all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap());
    let lists = |r: &Run| ids.map(|id| test_clip::effects(r.state.sequences[0].all_clips().find(|c| c.id == id).unwrap()));
    let before = pictures(&run);
    let cmds = crate::effect_targets::all_or_none(&targets, |id| crate::browser::effects_tab::effect_commands_in(&run.state, &chain, id)).unwrap();
    assert!(cmds.iter().all(|c| matches!(c, AppCommand::AddNodeAt { .. } | AppCommand::BeginHistoryBatch | AppCommand::EndHistoryBatch)), "{name}: by path: {cmds:?}");
    for cmd in cmds {
        run.apply(cmd);
    }
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    let want: Vec<BuiltinEffect> = std::iter::once(BuiltinEffect::GaussianBlur { radius: 3.0 }).chain(effects.iter().cloned()).collect();
    for list in lists(&run) {
        assert_eq!(serde_json::to_value(&list).unwrap(), serde_json::to_value(&want).unwrap(), "{name}: the whole chain, in order, on each clip");
    }
    assert_eq!(run.state.active_history().undo_count(), 1, "{name}: one step");
    assert!(run.step(false));
    assert!(pictures(&run) == before, "{name}: undone on both");
}

/// Start a real drag on the header's Mix: its label, a number row's
/// scrubbing handle, pressed and dragged left (from full, its top).
fn start_header_mix_drag(run: &mut Run) -> egui::Pos2 {
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    let at = drawn
        .iter()
        .find(|(text, _)| text == "Mix")
        .map(|(_, at)| *at)
        .unwrap_or_else(|| panic!("Mix label: {drawn:?}"));
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [-6.0, -12.0, -18.0] {
        run.frame(
            vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))],
            egui::Modifiers::NONE,
        );
    }
    at + egui::vec2(-18.0, 0.0)
}

#[test]
fn a_header_mix_drag_is_one_exact_undo_step() {
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }));
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
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }));
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

/// Escape during an effect row's number drag takes it back: the whole
/// picture as it was, and no history.
#[test]
fn escape_cancels_a_number_drag() {
    for (effect, label) in [
        (BuiltinEffect::GaussianBlur { radius: 3.0 }, "Radius"),
        (BuiltinEffect::TemporalDenoise { strength: 0.5, threshold: 0.02 }, "Strength"),
    ] {
        let mut run = Run::new(fixture(&effect));
        let before = run.clip().picture_v2().unwrap();
        run.frame(Vec::new(), egui::Modifiers::NONE);
        let (_, at) = run.row(label).expect("numeric row");
        run.button(at, egui::PointerButton::Primary, true);
        for dx in [6.0, 12.0, 18.0] {
            run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
        }
        assert_ne!(run.clip().picture_v2().unwrap(), before, "the real control must edit before Escape");
        run.key(egui::Key::Escape, egui::Modifiers::NONE);
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(28.0, 0.0))], egui::Modifiers::NONE);
        run.button(at + egui::vec2(28.0, 0.0), egui::PointerButton::Primary, false);
        assert_eq!(run.clip().picture_v2().unwrap(), before, "{label}: Escape");
        assert_eq!(run.state.active_history().undo_count(), 0);
        assert!(!run.state.any_drag_in_progress());
        assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    }
}

#[test]
fn perspective_label_reset_restores_unset_below_its_soft_range() {
    let mut transform = crate::inspector::video_tab::TransformParams {
        perspective: 1.7, rotation_y: 15.0, ..Default::default()
    };
    let clip = fixture(&transform.to_builtin_effect());
    let mut run = Run::new(clip.clone());
    let mut oracle = Run::new(clip);
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
        let mut run = Run::new(clip.clone());
        let mut oracle = Run::new(clip);
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
            let mut run = Run::new(fixture(&effect));
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
    let mut run = Run::new(fixture(&BuiltinEffect::Opacity { value: 0.8 }));
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
fn soft_edge_alias_reads_and_edits_its_existing_animation() {
    let clip = soft_edge_clip(true);
    let before = clip.picture_v2().unwrap();
    let mut new = Run::new(clip.clone());
    let mut oracle = clip;
    new.frame(Vec::new(), egui::Modifiers::NONE);
    let (_, number) = new.row("Soft Edge").unwrap();
    let drawn = new.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(drawn.iter().any(|(s, p)| p.distance(number) < 1.0 && s.parse::<f32>() == Ok(0.4)), "the existing key is shown");
    assert!(play(&mut new, "Soft Edge", Script::Type, "0.700"));
    // Use the clip's existing legacy lane API as the reference: the
    // shared row driver does not dispatch old keyframe commands.
    let effect = test_clip::effect_id(&oracle, 0);
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
        let mut run = Run::new(soft_edge_clip(keyed));
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

/// A clip with no Opacity effect yet, its Video tab dragging the clip's
/// own opacity (#1364). Escape takes the whole drag back; a press and
/// release changes nothing; a drag commits, and undo takes it all back.
#[test]
fn a_clip_without_opacity_cancels_commits_and_undoes_its_first_drag() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let bare = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let start = |run: &mut Run| {
        run.property = true;
        run.frame(Vec::new(), egui::Modifiers::NONE);
        run.row("Opacity").expect("the Video tab's Opacity row").1
    };
    // Escape mid-drag.
    let mut run = Run::new(bare.clone());
    let before = run.clip().picture_v2().unwrap();
    let at = start(&mut run);
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [-6.0, -12.0, -18.0] {
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
    }
    assert_ne!(run.clip().picture_v2().unwrap(), before, "the drag edits");
    run.key(egui::Key::Escape, egui::Modifiers::NONE);
    run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(-30.0, 0.0))], egui::Modifiers::NONE);
    run.button(at + egui::vec2(-30.0, 0.0), egui::PointerButton::Primary, false);
    assert_eq!(run.clip().picture_v2().unwrap(), before, "Escape leaves nothing behind");
    assert_eq!(run.state.active_history().undo_count(), 0);
    assert!(!run.state.any_drag_in_progress());
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    // A press and a release, nothing moved.
    let mut run = Run::new(bare.clone());
    let at = start(&mut run);
    run.button(at, egui::PointerButton::Primary, true);
    run.button(at, egui::PointerButton::Primary, false);
    run.frame(Vec::new(), egui::Modifiers::NONE);
    assert_eq!(run.clip().picture_v2().unwrap(), before, "a click changes nothing");
    assert_eq!(run.state.active_history().undo_count(), 0);
    // A drag commits, and undo takes all of it back.
    let mut run = Run::new(bare.clone());
    let at = start(&mut run);
    run.button(at, egui::PointerButton::Primary, true);
    for dx in [-6.0, -12.0, -18.0] {
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
    }
    run.button(at + egui::vec2(-18.0, 0.0), egui::PointerButton::Primary, false);
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let after = run.clip().picture_v2().unwrap();
    assert_ne!(after, before, "the drag is kept");
    assert!(!run.state.any_drag_in_progress());
    // One step (IC13).
    assert_eq!(run.state.active_history().undo_count(), 1);
    assert!(run.step(false));
    assert_eq!(run.clip().picture_v2().unwrap(), before, "undo takes it all back");
    assert!(run.step(true));
    assert_eq!(run.clip().picture_v2().unwrap(), after, "redo puts it back exactly");
    assert!(run.step(false));
    assert_eq!(run.clip().picture_v2().unwrap(), before, "undo takes it all back");
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
}

/// A first drag that adds Opacity and ends unseen by its row (released
/// while the row isn't drawn) leaves no mark of its add. Later the user
/// removes that effect and adds their own; a drag on it cancelled by
/// Escape keeps it, though an add is the last step in the history (#1364).
#[test]
fn an_unseen_end_of_an_adding_drag_never_lets_escape_remove_a_kept_opacity() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let bare = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut run = Run::new(bare);
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
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }));
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
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }));
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Opacity { value: 0.5 } });
    let opacity = node_id_of(&run, "klipp.legacy.opacity").expect("Opacity added");
    let blur = node_id_of(&run, "klipp.legacy.effect").expect("the Blur");
    // A later add of a plain effect: after Opacity it can't go through the
    // picture's ops either (a picture-op add needs a plain effect last), so
    // it too is recorded as the effect list.
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Vignette { amount: 0.4, size: 0.7, softness: 0.4, roundness: 0.0 } });
    assert!(run.step(false), "undo the later add");
    assert_eq!(node_id_of(&run, "klipp.legacy.opacity"), Some(opacity), "Opacity keeps its id");
    assert!(run.clip().picture_v2().unwrap().picture.graph.nodes.iter().any(|n| n.id == blur), "the Blur keeps its id");
    assert!(run.step(true), "redo it");
    assert_eq!(node_id_of(&run, "klipp.legacy.opacity"), Some(opacity));
    assert!(run.clip().picture_v2().unwrap().picture.graph.nodes.iter().any(|n| n.id == blur));
}


/// #3200: an effect added with a frei0r id no installed plug-in is (a
/// preset or paste from another machine) is refused and said, the clip
/// left as it was; one whose name is in another case, or whose folder is
/// another machine's, is added as the installed plug-in's own id.
#[test]
fn a_frei0r_effect_is_added_only_by_an_installed_plug_ins_id() {
    klipp_effects::frei0r_effect::_register_named_for_test("/usr/lib/frei0r-1/klipp3200ui.so", "Klipp3200Ui");
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut run = Run::new(klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0)));
    let before = run.clip().picture_v2().unwrap();
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Frei0r { type_id: "frei0r:/elsewhere/nothing3200.so:Nothing3200".into(), params: Vec::new() } });
    assert_eq!(run.clip().picture_v2().unwrap(), before, "refused: the clip as it was");
    assert!(run.state.toast.message_for_test().is_some_and(|m| m.contains("not added") && m.contains("klipp3200ui.so")), "said, naming what is installed: {:?}", run.state.toast.message_for_test());
    run.apply(AppCommand::AddBuiltinEffect { clip_id: run.clip, effect: BuiltinEffect::Frei0r { type_id: "frei0r:/usr/lib64/frei0r-1/klipp3200ui.so:klipp3200ui".into(), params: Vec::new() } });
    let added: Vec<String> = test_clip::effects(run.clip()).into_iter().filter_map(|e| match e {
        BuiltinEffect::Frei0r { type_id, .. } => Some(type_id),
        _ => None,
    }).collect();
    assert_eq!(added, ["frei0r:/usr/lib/frei0r-1/klipp3200ui.so:Klipp3200Ui"], "added as the installed plug-in's own id");
}
