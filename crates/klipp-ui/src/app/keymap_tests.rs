//! Every built-in keymap, checked as the keys arrive (#2934): no two
//! actions on one key where both are read, every binding pressed as a
//! keyboard sends it reaches its own action and no other in its place, every
//! bound action is one the dispatcher reads, and each action is read where
//! `keybindings::action_places` says.

use klipp_core::AppCommand;
use crate::keybindings::{action_places, resolved, shared_place, KeyBindings, KeyCombo, KeymapPreset, Place, APP_ACTIONS, CTRL_ACTIONS, GRADE_ACTIONS, TRANSPORT_ACTIONS};

/// What a US keyboard types for `key` with Shift held or not, when it
/// types anything.
fn typed(key: &str, shift: bool) -> Option<String> {
    let c = key.chars().next().filter(|_| key.chars().count() == 1)?;
    if c.is_ascii_alphabetic() {
        return Some(if shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }.to_string());
    }
    if !shift {
        return Some(c.to_string());
    }
    let pairs = [('1', '!'), ('2', '@'), ('3', '#'), ('4', '$'), ('5', '%'), ('6', '^'), ('7', '&'), ('8', '*'), ('9', '('), ('0', ')'),
        (',', '<'), ('.', '>'), ('/', '?'), (';', ':'), ('\'', '"'), ('[', '{'), (']', '}'), ('\\', '|'), ('-', '_'), ('=', '+'), ('`', '~')];
    pairs.iter().find(|(k, _)| *k == c).map(|(_, s)| s.to_string())
}

/// The events a keyboard sends for `combo`: the key, and the text it
/// types when no Ctrl or Alt is held.
fn events(combo: &KeyCombo) -> (Vec<egui::Event>, egui::Modifiers) {
    let modifiers = egui::Modifiers { ctrl: combo.ctrl, shift: combo.shift, alt: combo.alt, command: combo.ctrl, ..Default::default() };
    let mut events = Vec::new();
    if let Some(key) = egui::Key::from_name(&combo.key) {
        events.push(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers });
    }
    if !combo.ctrl && !combo.alt {
        if let Some(text) = typed(&combo.key, combo.shift) {
            events.push(egui::Event::Text(text));
        }
    }
    (events, modifiers)
}

/// The actions `kb` reads as pressed when `combo` is.
fn fired(ctx: &egui::Context, kb: &KeyBindings, combo: &KeyCombo) -> Vec<String> {
    let (events, modifiers) = events(combo);
    let mut on = Vec::new();
    let _ = ctx.run(egui::RawInput { events, modifiers, ..Default::default() }, |ctx| {
        ctx.input(|i| on = kb.bindings.keys().filter(|a| kb.is_action_pressed(a, i)).cloned().collect());
    });
    on
}

#[test]
fn no_built_in_keymap_puts_two_actions_on_one_key_in_one_place() {
    for preset in KeymapPreset::ALL {
        let conflicts = KeyBindings::from_preset(preset).find_conflicts();
        assert!(conflicts.is_empty(), "{}: {:#?}", preset.label(), conflicts);
    }
}

/// Each binding of each keymap, pressed: its own action runs, and no other
/// action read in the same place, but for the pairs the dispatcher tells
/// apart (`keybindings::resolved`).
#[test]
fn every_binding_of_every_keymap_runs_its_own_action() {
    let ctx = egui::Context::default();
    for preset in KeymapPreset::ALL {
        let kb = KeyBindings::from_preset(preset);
        let mut wrong = Vec::new();
        for (action, combos) in &kb.bindings {
            for combo in combos {
                let on = fired(&ctx, &kb, combo);
                if !on.contains(action) {
                    wrong.push(format!("{} does not run {action}", combo.display()));
                }
                for other in on.iter().filter(|o| *o != action) {
                    if shared_place(action, other).is_some() && resolved(action, other).is_none() {
                        wrong.push(format!("{} runs {action} and {other}", combo.display()));
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{}:\n{}", preset.label(), wrong.join("\n"));
    }
}

/// The sources that read the actions' keys.
const DISPATCH: &[&str] = &[include_str!("shortcuts.rs"), include_str!("../panels/transport.rs"), include_str!("../multicam_grid.rs")];

/// The actions read in `function` of shortcuts.rs.
fn read_in(function: &str) -> Vec<String> {
    let src = include_str!("shortcuts.rs");
    let start = src.find(&format!("fn {function}(")).unwrap_or_else(|| panic!("no {function}"));
    let rest = &src[start + 3..];
    let end = ["\nfn ", "\n    fn ", "\npub(super) fn ", "\npub(crate) fn "].iter().filter_map(|m| rest.find(m)).min().map_or(src.len(), |e| start + 3 + e);
    let body = &src[start..end];
    body.match_indices("is_action_pressed(\"").map(|(at, m)| body[at + m.len()..].split('"').next().unwrap_or_default().to_string()).collect()
}

/// Every action a keymap binds is read by the dispatcher (a binding that
/// names an action nothing reads fails), and the place lists are the
/// dispatcher's own: the app keys in `shortcuts_app`, the Ctrl keys in
/// `shortcuts_ctrl_global`, the grade's in `grade_node_keys`, playback in
/// the transport panel.
#[test]
fn every_bound_action_is_read_where_its_places_say() {
    for preset in KeymapPreset::ALL {
        for action in KeyBindings::from_preset(preset).bindings.keys() {
            // A numbered family is read by its made name (`multicam_angle_{angle}`).
            let family = action.trim_end_matches(|c: char| c.is_ascii_digit());
            let read = DISPATCH.iter().any(|src| src.contains(&format!("\"{action}\"")) || (family != action && src.contains(&format!("\"{family}{{"))));
            assert!(read, "{}: {action} is bound but nothing reads it", preset.label());
        }
    }
    let same = |list: &[&str], function: &str| {
        let mut want: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        let mut got = read_in(function);
        want.sort();
        got.sort();
        got.dedup();
        assert_eq!(got, want, "{function}");
    };
    same(APP_ACTIONS, "shortcuts_app");
    same(CTRL_ACTIONS, "shortcuts_ctrl_global");
    same(GRADE_ACTIONS, "grade_node_keys");
    let transport = include_str!("../panels/transport.rs");
    for action in TRANSPORT_ACTIONS {
        assert!(transport.contains(&format!("is_action_pressed(\"{action}\"")), "{action} in the transport");
    }
    assert_eq!(action_places("trim_edit_forward"), [Place::Editor]);
}

/// The pairs that share a key by design: one key runs one of them, as
/// `keybindings::resolved` says.
#[test]
fn a_shared_key_runs_one_of_its_pair() {
    use crate::state::PanelKind;
    let kb = KeyBindings::defaults();
    let ctx = egui::Context::default();
    let clip = klipp_core::ClipId::new();
    let press = |key: egui::Key, ctrl: bool, f: &mut dyn FnMut(&egui::InputState)| {
        let modifiers = egui::Modifiers { ctrl, command: ctrl, ..Default::default() };
        let input = egui::RawInput { events: vec![egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }], modifiers, ..Default::default() };
        let _ = ctx.run(input, |ctx| ctx.input(|i| f(i)));
    };
    // Ctrl+C: the clip in the timeline, the nodes in the colour panel
    // with a node selected.
    let mut state = crate::state::AppState { selected_clip: Some(clip), active_panel: Some(PanelKind::Timeline), ..Default::default() };
    let mut got = None;
    press(egui::Key::C, true, &mut |i| got = super::shortcuts::clipboard_copy(&state, &kb, i));
    assert!(matches!(got, Some(AppCommand::CopyClip { .. })), "{got:?}");
    state.active_panel = Some(PanelKind::NodeInspector);
    state.selected_node_id = Some(klipp_nodes::NodeId::new_v4());
    press(egui::Key::C, true, &mut |i| got = super::shortcuts::clipboard_copy(&state, &kb, i));
    assert!(matches!(got, Some(AppCommand::CopyColorNodes { .. })), "{got:?}");
    press(egui::Key::V, true, &mut |i| got = super::shortcuts::clipboard_paste(&state, &kb, i));
    assert!(matches!(got, Some(AppCommand::PasteClip)), "nothing on the grade clipboard: the clip's paste: {got:?}");
    // ]: with an edit point selected the trim alone; the grade's keys
    // leave the node selection where it was.
    let mut seq = klipp_timeline::Sequence::new_default();
    let mut c = klipp_timeline::Clip::gap(klipp_core::Timecode::from_frames(0, 24), klipp_core::Timecode::from_frames(48, 24));
    c.id = clip;
    seq.tracks[0].clips = vec![c];
    let seq_track = seq.tracks[0].id;
    let mut state = crate::state::AppState { sequences: vec![seq], selected_clip: Some(clip), ..Default::default() };
    state.selected_edits.push(crate::edit_point::EditPoint { track: seq_track, outgoing: Some(clip), incoming: None, side: crate::edit_point::EditSide::Both });
    let mut cmds = Vec::new();
    let before = state.selected_node_id;
    press(egui::Key::CloseBracket, false, &mut |i| super::shortcuts::editor_grade_keys(&mut state, &kb, i, &mut cmds));
    assert_eq!(state.selected_node_id, before, "the trim's key, not the grade's");
}

/// Each editor layout is its editor's published default (#2934): every
/// action the fixture matches has exactly the keys the editor documents
/// (none where it documents none), but for the fixture's deviations, each
/// with its reason; no Klipp action keeps a key the editor uses for a
/// command Klipp lacks; and every row of the table is the fixture's.
#[test]
fn every_editor_layout_is_its_editors_published_default() {
    let fixtures = [
        (KeymapPreset::Premiere, include_str!("../keymap_fixtures/premiere.json")),
        (KeymapPreset::Avid, include_str!("../keymap_fixtures/avid.json")),
        (KeymapPreset::Resolve, include_str!("../keymap_fixtures/resolve.json")),
        (KeymapPreset::FinalCut, include_str!("../keymap_fixtures/final_cut.json")),
    ];
    for (preset, text) in fixtures {
        let fx: serde_json::Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("{}: {e}", preset.label()));
        assert!(fx["sources"].as_array().is_some_and(|s| !s.is_empty() && s.iter().all(|s| s["url"].as_str().is_some_and(|u| u.starts_with("https://")))), "{}: sources", preset.label());
        let bound = preset.bindings();
        let keys = |a: &str| -> Vec<String> { bound.get(a).map(|c| c.iter().map(KeyCombo::display).collect()).unwrap_or_default() };
        let deviations = fx["deviations"].as_object().cloned().unwrap_or_default();
        let mut wrong = Vec::new();
        for (action, row) in fx["actions"].as_object().expect("actions") {
            let documented: Vec<String> = row["keys"].as_array().unwrap().iter().map(|k| KeyCombo::parse(k.as_str().unwrap()).expect("a key").display()).collect();
            let have = keys(action);
            match deviations.get(action) {
                Some(d) => {
                    assert!(d["why"].as_str().is_some_and(|w| w.len() > 20), "{}: {action}'s deviation says why", preset.label());
                    if documented.iter().any(|k| have.contains(k)) {
                        wrong.push(format!("{action} deviates but holds a documented key: {have:?}"));
                    }
                }
                None if have != documented => wrong.push(format!("{action}: {have:?}, documented {documented:?} ({})", row["command"])),
                None => {}
            }
        }
        for key in preset.taken() {
            let combo = KeyCombo::parse(key).unwrap().display();
            for (action, combos) in &bound {
                if combos.iter().any(|c| c.display() == combo) && fx["actions"].get(action).is_none() {
                    wrong.push(format!("{action} keeps {combo}, the editor's for a command Klipp lacks"));
                }
            }
        }
        for (action, _) in crate::keymap_presets::rows(preset) {
            if fx["actions"].get(*action).is_none() {
                wrong.push(format!("{action} is in the table but not the fixture"));
            }
        }
        assert!(wrong.is_empty(), "{}:\n{}", preset.label(), wrong.join("\n"));
    }
}

/// The shared keys' picks (`shortcuts::shared_key_losers`), each both
/// ways: by the edit points, the clip and the viewer shown.
#[test]
fn a_shared_key_is_given_by_the_selection() {
    let ctx = egui::Context::default();
    let kb = KeyBindings::from_preset(KeymapPreset::Resolve);
    let losers = |state: &crate::state::AppState, key: &str| {
        let combo = KeyCombo::parse(key).unwrap();
        let (events, modifiers) = events(&combo);
        let mut out = Vec::new();
        let _ = ctx.run(egui::RawInput { events, modifiers, ..Default::default() }, |ctx| ctx.input(|i| out = super::shortcuts::shared_key_losers(state, &kb, i)));
        out
    };
    let mut state = crate::state::AppState::default();
    assert_eq!(losers(&state, ","), ["trim_edit_back"], "no edit point: a nudge");
    state.selected_edits.push(crate::edit_point::EditPoint { track: klipp_core::TrackId::new(), outgoing: None, incoming: None, side: crate::edit_point::EditSide::Both });
    assert_eq!(losers(&state, ","), ["nudge_clips_back"], "an edit point: a trim");
    assert_eq!(losers(&state, "Delete"), ["extract_edit"], "Delete takes the clip or the range itself");
    assert_eq!(losers(&state, "Backspace"), ["lift_clip"], "no clip: the range lifted");
    assert_eq!(losers(&state, "M"), ["add_clip_marker"], "no clip: a timeline marker");
    assert_eq!(losers(&state, "F"), ["match_frame_source_to_program"], "the timeline shown: to its source");
    state.selected_clip = Some(klipp_core::ClipId::new());
    state.viewer_mode = klipp_core::ViewerMode::Source;
    assert_eq!(losers(&state, "Backspace"), ["lift_edit"], "a clip: lifted");
    assert_eq!(losers(&state, "M"), ["add_marker"], "a clip: its marker");
    assert_eq!(losers(&state, "F"), ["match_frame_program_to_source"], "the source shown: to the timeline");
    // Bound apart, as in Klipp's own layout, nothing is left out.
    let own = KeyBindings::defaults();
    let combo = KeyCombo::parse(",").unwrap();
    let (events, modifiers) = events(&combo);
    let mut out = vec!["x"];
    let _ = ctx.run(egui::RawInput { events, modifiers, ..Default::default() }, |ctx| ctx.input(|i| out = super::shortcuts::shared_key_losers(&state, &own, i)));
    assert!(out.is_empty(), "{out:?}");
}

/// #2943: while a field has the keys no editing shortcut runs (Ctrl+A is
/// the field's, not Select All); a field left by Enter keeps the letters
/// from the editor (B does not arm the Blade) until a click anywhere or
/// Escape lets them go.
#[test]
fn a_field_keeps_the_editors_keys_while_typed_in_and_after_enter() {
    use super::shortcuts::{field_keys_held, key_scope, KeyScope};
    let mut state = crate::state::AppState { project_path: Some(crate::project_paths::UNTITLED.into()), active_panel: Some(crate::state::PanelKind::Timeline), ..Default::default() };
    assert_eq!(key_scope(&state, false), KeyScope::All);
    assert_eq!(key_scope(&state, true), KeyScope::CtrlOnly, "typing in a field: the Ctrl keys only");
    assert!(!CTRL_ACTIONS.contains(&"select_all") && action_places("select_all") == [Place::Editor], "Select All is not read while a field types");
    assert!(action_places("blade_tool") == [Place::Editor]);
    // A real number field: focused, a value typed, then Enter.
    let ctx = egui::Context::default();
    let mut value = 100.0_f32;
    let id_of = std::cell::Cell::new(egui::Id::NULL);
    let run = |events: Vec<egui::Event>, value: &mut f32| -> (bool, bool, bool) {
        let mut out = (false, false, false);
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))), events, ..Default::default() };
        let _ = ctx.run(input, |ctx| {
            let before = ctx.memory(|m| m.focused().is_some());
            egui::CentralPanel::default().show(ctx, |ui| {
                let r = ui.add(egui::DragValue::new(value));
                id_of.set(r.id);
            });
            let now = ctx.memory(|m| m.focused().is_some());
            out = (before, now, false);
            out.2 = ctx.input(|i| field_keys_held(false, before, now, i));
        });
        out
    };
    run(Vec::new(), &mut value);
    ctx.memory_mut(|m| m.request_focus(id_of.get()));
    run(Vec::new(), &mut value);
    run(vec![egui::Event::Text("5".into())], &mut value);
    let key = |key| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    let (before, now, held) = run(vec![key(egui::Key::Enter)], &mut value);
    assert!(before && !now, "Enter left the field");
    assert!(held, "and holds the single keys");
    state.field_keys_held = held;
    assert_eq!(key_scope(&state, false), KeyScope::CtrlOnly, "so B is no Blade");
    // A click anywhere, or Escape, lets them go.
    let input = egui::RawInput { events: vec![egui::Event::PointerButton { pos: egui::pos2(300.0, 150.0), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }], ..Default::default() };
    let mut after_click = true;
    let _ = ctx.run(input, |ctx| after_click = ctx.input(|i| field_keys_held(true, false, false, i)));
    assert!(!after_click, "a click lets them go");
    let mut after_escape = true;
    let _ = ctx.run(egui::RawInput { events: vec![key(egui::Key::Escape)], ..Default::default() }, |ctx| after_escape = ctx.input(|i| field_keys_held(true, false, false, i)));
    assert!(!after_escape, "Escape too");
    let mut kept = false;
    let _ = ctx.run(egui::RawInput { events: vec![egui::Event::Text("b".into())], ..Default::default() }, |ctx| kept = ctx.input(|i| field_keys_held(true, false, false, i)));
    assert!(kept, "a letter does not");
}
