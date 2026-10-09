//! M3 step 18 parity: the keyframe curve editor (d9's family), on an
//! effect parameter's track, as it lists them today (tracks that have
//! keys). Each case drives the editor, and the keyframe menu the timeline
//! draws for it, with the generated inspector off and on, and requires the
//! same picture, one undo step and an exact undo in each.
//!
//! Previous/Next key doesn't follow the curve editor's track in either view
//! (#1401), and grade tracks aren't listed (#891): both are central
//! exclusions in the parity rules.

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use super::parity_manifest::{catalog, CatalogValue, CoverageKey, Interaction, KeyInterpolation, Manifest, Report, Surface};
use crate::panels::curve_editor::{CurveEditorPanel, CurveParam};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_effects::BuiltinEffect;

struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// The effect whose track the cases edit, and the keys it starts with.
const TAG: &str = "gaussian_blur";
const PARAM: &str = "radius";
const KEYS: [(f64, f32); 2] = [(0.5, 2.0), (1.5, 8.0)];

/// One clip, selected, its blur's radius keyed at `KEYS`, drawn in the
/// curve editor on that track.
struct Curves {
    state: AppState,
    clip: ClipId,
    panel: CurveEditorPanel,
    ctx: egui::Context,
    clock: f64,
    unapplied: Vec<String>,
}

/// The clip the cases edit: its blur's radius keyed at `KEYS`, with no
/// history. Built once per case, so both views start from the same clip.
fn fixture() -> Result<klipp_timeline::Clip, String> {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.push_effect(BuiltinEffect::GaussianBlur { radius: 3.0 });
    let id = clip.id;
    let mut state = AppState::default();
    state.sequences[0].tracks[0].clips.push(clip);
    let node = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().picture_v2()?.picture.graph.nodes.iter()
        .find(|n| klipp_graph_lift::effect_params::tag(n) == Some(TAG)).map(|n| n.id).ok_or("no blur node")?;
    let addr = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: id.0 }, [node]);
    for (t, v) in KEYS {
        let keyframe = serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) }).unwrap();
        state.path_command(&AppCommand::AddKeyAt { addr: addr.clone(), key: PARAM.into(), keyframe }).ok_or("no key added")?;
    }
    let clip = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().clone();
    Ok(clip)
}

impl Curves {
    /// `clip` selected in a fresh state, drawn in the curve editor on its
    /// track, with the generated inspector `generated`.
    fn over(clip: klipp_timeline::Clip, generated: bool) -> Result<Self, String> {
        let id = clip.id;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.use_generated_stack = generated;
        state.selected_clip = Some(id);
        let mut panel = CurveEditorPanel::new();
        let (path, key, tag) = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().animated_effect_keys().into_iter().next().ok_or("no animated track")?;
        panel.show_track(CurveParam::Node { path, key, tag });
        let mut run = Curves { state, clip: id, panel, ctx: egui::Context::default(), clock: 0.0, unapplied: Vec::new() };
        run.frame(Vec::new());
        run.frame(Vec::new());
        Ok(run)
    }

    fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    /// A fresh history over the clip as it is: where a case starts when
    /// its fixture needed an edit first.
    fn restart(&mut self) {
        let clip = self.clip().clone();
        let generated = self.state.use_generated_stack;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.use_generated_stack = generated;
        state.selected_clip = Some(self.clip);
        self.state = state;
        self.frame(Vec::new());
    }

    fn picture(&self) -> klipp_timeline::LiftedPicture {
        (*self.clip().picture_v2().unwrap()).clone()
    }

    /// The track's keys, as (time, value, interpolation).
    fn keys(&self) -> Vec<(f64, f32, klipp_core::Interpolation)> {
        let CurveParam::Node { path, key, .. } = self.panel_param() else { return Vec::new() };
        self.clip().node_key_state(&path, &key).map(|s| s.keys.iter().map(|k| (k.time_secs, k.value, k.interpolation)).collect()).unwrap_or_default()
    }

    fn panel_param(&self) -> CurveParam {
        let (path, key, tag) = self.clip().animated_effect_keys().into_iter().next().expect("the track");
        CurveParam::Node { path, key, tag }
    }

    /// One frame with `events`: the curve editor and the keyframe menu, as
    /// the app draws them, and their commands applied as it applies them.
    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))), time: Some(self.clock), events, ..Default::default() };
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let Curves { state, panel, ctx, .. } = self;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                cmds.extend(panel.show(ui, state, &theme));
                crate::panels::timeline::TimelinePanel::show_keyframe_context_menu(ui, state, &mut cmds);
            });
        });
        for cmd in cmds {
            self.apply(cmd);
        }
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    fn apply(&mut self, cmd: AppCommand) {
        self.state.before_dispatch(&cmd);
        self.state.toast = Default::default();
        if self.state.path_command(&cmd).is_some() {
            if let Some(why) = self.state.toast.message_for_test() {
                self.unapplied.push(format!("{cmd:?} refused: {why}"));
            }
            return;
        }
        // The playhead and the view: nothing the clip records.
        if matches!(cmd, AppCommand::SeekTo(_)) {
            return;
        }
        if !(InspectorEditHost { state: &mut self.state, side: &Sink }).apply(&cmd) {
            self.unapplied.push(format!("{cmd:?}"));
        }
    }

    fn at(&self, t: f64, v: f32) -> egui::Pos2 {
        self.panel.screen_at(t, v).expect("the graph is drawn")
    }

    fn button(&mut self, at: egui::Pos2, button: egui::PointerButton, pressed: bool) {
        self.frame(vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }]);
    }

    fn click(&mut self, at: egui::Pos2, button: egui::PointerButton) {
        self.button(at, button, true);
        self.button(at, button, false);
        self.frame(vec![egui::Event::PointerGone]);
    }

    fn step(&mut self, forward: bool) -> Result<(), String> {
        self.state.before_dispatch(&if forward { AppCommand::Redo } else { AppCommand::Undo });
        let history = self.state.active_history_mut();
        let entry = if forward { history.redo() } else { history.undo() }.ok_or("no step to take")?;
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut self.state.sequences, active: self.state.active_sequence_idx, media_pool: &mut self.state.media_pool, fps: &mut self.state.fps };
        if forward { doc.apply_forward(&entry.command); } else { doc.apply_inverse(&entry.command); }
        Ok(())
    }
}

/// The effect's track entry, required on the curve editor.
fn entry() -> Option<super::parity_manifest::CatalogEntry> {
    let ty = klipp_effects::registry::effect_type(TAG)?;
    catalog().into_iter().find(|e| e.type_id == ty.desc.id && e.parameter == PARAM && matches!(e.value, CatalogValue::Parameter { .. }))
}

pub(super) fn require(manifest: &mut Manifest) {
    if let Some(e) = entry() {
        manifest.require_fixture(&e, Surface::CurveEditor, "keyed");
    }
}

/// One edit through the editor in both views: `want` on its keys, the same
/// picture in each, one step, undone and redone exactly.
fn case(want: impl Fn(&[(f64, f32, klipp_core::Interpolation)]) -> Result<(), String>, drive: impl Fn(&mut Curves) -> Result<(), String>) -> Result<(), String> {
    case_from(|_| Ok(()), want, drive)
}

/// As [`case`], from where `prepare` leaves the fixture, with a fresh
/// history.
fn case_from(prepare: impl Fn(&mut Curves) -> Result<(), String>, want: impl Fn(&[(f64, f32, klipp_core::Interpolation)]) -> Result<(), String>, drive: impl Fn(&mut Curves) -> Result<(), String>) -> Result<(), String> {
    let mut pictures = Vec::new();
    let clip = fixture()?;
    for generated in [false, true] {
        let mut c = Curves::over(clip.clone(), generated)?;
        prepare(&mut c).map_err(|e| format!("generated {generated}: preparing: {e}"))?;
        c.restart();
        let before = c.picture();
        drive(&mut c).map_err(|e| format!("generated {generated}: {e}"))?;
        if !c.unapplied.is_empty() {
            return Err(format!("generated {generated}: unapplied {:?}", c.unapplied));
        }
        want(&c.keys()).map_err(|e| format!("generated {generated}: {e}: {:?}", c.keys()))?;
        if c.state.active_history().undo_count() != 1 {
            return Err(format!("generated {generated}: {} steps, not one", c.state.active_history().undo_count()));
        }
        let after = c.picture();
        c.step(false)?;
        if c.picture() != before {
            return Err(format!("generated {generated}: undo doesn't give back the picture"));
        }
        c.step(true)?;
        if c.picture() != after {
            return Err(format!("generated {generated}: redo doesn't give back the picture"));
        }
        pictures.push(after);
    }
    if pictures[0] != pictures[1] {
        return Err("the two views' pictures differ".into());
    }
    Ok(())
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

pub(super) fn run(report: &mut Report) {
    let Some(e) = entry() else {
        report.case("curve editor", [], || Err("no catalog entry for the blur's radius".into()));
        return;
    };
    let key = |interaction| CoverageKey::new(e.type_id, &e.parameter, Surface::CurveEditor, "keyed", interaction);

    report.case("curve editor opened", [key(Interaction::OpenWithoutWrite)], || {
        for generated in [false, true] {
            let mut c = Curves::over(fixture()?, generated)?;
            let before = c.picture();
            c.frame(Vec::new());
            if c.picture() != before || c.state.active_history().undo_count() != 0 || !c.unapplied.is_empty() {
                return Err(format!("generated {generated}: opening wrote"));
            }
        }
        Ok(())
    });

    // A double-click where there's no key adds one there, at that value.
    report.case("curve editor adds a key", [key(Interaction::AddKey), key(Interaction::UndoRedo)], || {
        case(|keys| if keys.len() == 3 && keys.iter().any(|k| near(k.0, 1.0, 0.02) && (k.1 - 5.0).abs() < 0.2) { Ok(()) } else { Err("no new key near (1.0, 5)".into()) }, |c| {
            let at = c.at(1.0, 5.0);
            c.button(at, egui::PointerButton::Primary, true);
            c.button(at, egui::PointerButton::Primary, false);
            c.button(at, egui::PointerButton::Primary, true);
            c.button(at, egui::PointerButton::Primary, false);
            c.frame(vec![egui::Event::PointerGone]);
            Ok(())
        })
    });

    // A key dragged moves, in one step.
    report.case("curve editor moves a key", [key(Interaction::EditKey)], || {
        case(|keys| if keys.len() == 2 && keys.iter().any(|k| near(k.0, 1.2, 0.03) && (k.1 - 6.0).abs() < 0.3) && !keys.iter().any(|k| near(k.0, 1.5, 0.01)) { Ok(()) } else { Err("the key isn't near (1.2, 6)".into()) }, |c| {
            let (from, to) = (c.at(1.5, 8.0), c.at(1.2, 6.0));
            c.button(from, egui::PointerButton::Primary, true);
            // Just past egui's drag threshold first, still on the key: the
            // editor finds the dragged key where the drag starts.
            c.frame(vec![egui::Event::PointerMoved(from + (to - from).normalized() * 7.0)]);
            for k in 1..=4 {
                let p = from + (to - from) * (k as f32 / 4.0);
                c.frame(vec![egui::Event::PointerMoved(p)]);
            }
            c.button(to, egui::PointerButton::Primary, false);
            c.frame(vec![egui::Event::PointerGone]);
            Ok(())
        })
    });

    // A key clicked, then Delete, as the shortcut does it.
    report.case("curve editor removes a key", [key(Interaction::RemoveKey)], || {
        case(|keys| if keys.len() == 1 && near(keys[0].0, 0.5, 0.01) { Ok(()) } else { Err("the key at 1.5 isn't the one taken".into()) }, |c| {
            let at = c.at(1.5, 8.0);
            c.click(at, egui::PointerButton::Primary);
            let sk = c.state.selected_keyframe.take().ok_or("clicking the key didn't select it")?;
            let cmd = super::shortcuts::remove_selected_keyframe(&c.state, &sk).ok_or("Delete sends nothing")?;
            c.apply(cmd);
            Ok(())
        })
    });

    // The key's interpolation from the menu its right-click opens.
    for (item, interp, want) in [
        ("  Linear", KeyInterpolation::Linear, klipp_core::Interpolation::Linear),
        ("  Bezier (Custom)", KeyInterpolation::Bezier, klipp_core::Interpolation::Bezier),
        ("  Step (Hold)", KeyInterpolation::Step, klipp_core::Interpolation::Step),
    ] {
        report.case(&format!("curve editor sets {}", item.trim()), [key(Interaction::Interpolation(interp))], || {
            // From another interpolation, so the pick changes it.
            let from = if want == klipp_core::Interpolation::Linear { Some("  Step (Hold)") } else { None };
            case_from(
                |c| from.map_or(Ok(()), |f| pick(c, f)),
                |keys| if keys.iter().any(|k| near(k.0, 0.5, 0.01) && k.2 == want) { Ok(()) } else { Err(format!("the key at 0.5 isn't {want:?}")) },
                |c| pick(c, item),
            )
        });
    }
}

/// Right-click the key at 0.5 and pick `item` from the menu it opens.
fn pick(c: &mut Curves, item: &str) -> Result<(), String> {
    let at = c.at(0.5, 2.0);
    c.click(at, egui::PointerButton::Secondary);
    if c.state.keyframe_context_menu.is_none() {
        return Err("the right-click opened no menu".into());
    }
    let drawn = c.frame(Vec::new());
    let item_at = drawn.into_iter().rev().find(|(t, _)| t == item).map(|(_, p)| p).ok_or_else(|| format!("{item:?} isn't in the menu"))?;
    c.click(item_at, egui::PointerButton::Primary);
    Ok(())
}

#[test]
fn effect_tracks_edit_alike_in_the_curve_editor() {
    let mut report = Report::default();
    report.family("curve editor", run);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require(&mut manifest);
    // Every key the family can earn, earned: not Pixels (another family's),
    // and not those excluded with a citation (Previous/Next, #1401).
    let missing: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels) && !manifest.excluded_keys.contains_key(*k) && !report.passed.contains(k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
}
