//! A clip's grade tracks in the keyframe curve editor (#891): a keyed
//! grade parameter is listed by its path through the grade group
//! (`[group, node]`) and named by its node's type; a parameter with no
//! keys can start a track from the header's "+ Track" menu (a key at the
//! playhead with its value now); and a grade key's menu sets its
//! interpolation or eases it, each one undo step. Driven with pointer
//! events on the editor and the key menu as the app draws them.

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use crate::panels::curve_editor::CurveEditorPanel;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId, Interpolation, KeyEase, Keyframe};
use klipp_graph_format::path::NodePath;
use klipp_nodes::{NodeId, NodeKind};

struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// A graded clip, selected, drawn in the curve editor and its key menu.
struct Run {
    state: AppState,
    clip: ClipId,
    node: NodeId,
    panel: CurveEditorPanel,
    ctx: egui::Context,
    clock: f64,
    unapplied: Vec<String>,
}

impl Run {
    /// A Corrector's clip, its exposure at 0.7, the playhead `at` seconds in.
    fn new(at: f64) -> Self {
        let (mut state, clip, node) = super::grade_inspector_tests::graded(NodeKind::Corrector).expect("a Corrector");
        let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
        klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().find(|n| n.id == node).unwrap().params.exposure = Some(0.7));
        state.selected_clip = Some(clip);
        let start = state.sequences[0].all_clips().find(|c| c.id == clip).unwrap().timeline_in;
        state.playhead = klipp_core::Timecode::from_secs_f64(start.to_secs_f64() + at, start.rate);
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        let mut run = Run { state, clip, node, panel: CurveEditorPanel::new(), ctx, clock: 0.0, unapplied: Vec::new() };
        run.frame(Vec::new());
        run
    }

    fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    /// The Corrector's path in the clip's picture.
    fn path(&self) -> NodePath {
        self.clip().grade_nodes().into_iter().map(|(p, ..)| p).find(|p| p.node() == Some(self.node)).expect("the Corrector by path")
    }

    fn keys(&self, key: &str) -> Vec<Keyframe> {
        self.clip().node_key_state(&self.path(), &key.into()).map(|s| s.keys).unwrap_or_default()
    }

    /// Key `key` at `times` by path, then a fresh history.
    fn keyed(mut self, key: &str, keys: &[(f64, f32)]) -> Self {
        let path = self.path();
        for (t, v) in keys {
            let keyframe = serde_json::to_value(Keyframe { time_secs: *t, value: *v, interpolation: Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }).unwrap();
            self.state.path_command(&AppCommand::AddKeyAt { addr: path.clone(), key: key.into(), keyframe }).expect("a path command");
        }
        let clip = self.clip().clone();
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequences[0].tracks[v1].clips.push(clip);
        state.selected_clip = self.state.selected_clip;
        state.playhead = self.state.playhead;
        self.state = state;
        self.frame(Vec::new());
        self
    }

    fn picture(&self) -> klipp_timeline::LiftedPicture {
        (*self.clip().picture_v2().unwrap()).clone()
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 500.0))), time: Some(self.clock), events, ..Default::default() };
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let Run { state, panel, ctx, .. } = self;
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
        // The key menu wraps its edits in one named step (#2374).
        match &cmd {
            AppCommand::SeekTo(_) => return,
            AppCommand::BeginHistoryBatch => return self.state.active_history_mut().begin_batch(),
            AppCommand::EndHistoryBatch => return self.state.active_history_mut().end_batch(),
            AppCommand::EndHistoryBatchAs { label } => return self.state.active_history_mut().end_batch_as(label),
            _ => {}
        }
        let mut host = InspectorEditHost { state: &mut self.state, side: &Sink };
        if !host.apply_keyframe_drag(&cmd) && !host.apply(&cmd) {
            self.unapplied.push(format!("{cmd:?}"));
        }
    }

    fn click(&mut self, at: egui::Pos2, button: egui::PointerButton) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }]);
        }
    }

    /// Click the text `label` drawn last.
    fn click_text(&mut self, label: &str) -> Result<(), String> {
        let at = self.frame(Vec::new()).into_iter().rev().find(|(t, _)| t.trim() == label).map(|(_, p)| p).ok_or_else(|| format!("no {label:?} drawn"))?;
        self.click(at, egui::PointerButton::Primary);
        Ok(())
    }

    /// Hover the text `label` drawn last (a submenu opens on hover).
    fn hover_text(&mut self, label: &str) -> Result<(), String> {
        let at = self.frame(Vec::new()).into_iter().rev().find(|(t, _)| t.trim() == label).map(|(_, p)| p).ok_or_else(|| format!("no {label:?} drawn"))?;
        for _ in 0..3 {
            self.frame(vec![egui::Event::PointerMoved(at)]);
        }
        Ok(())
    }

    fn step(&mut self, forward: bool) -> Result<(), String> {
        self.state.before_dispatch(&if forward { AppCommand::Redo } else { AppCommand::Undo });
        // The step the app takes: the sequence's or the project's (#1986).
        let entry = if forward { self.state.take_redo() } else { self.state.take_undo() }.ok_or("no step to take")?;
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut self.state.sequences, active: self.state.active_sequence_idx, media_pool: &mut self.state.media_pool, fps: &mut self.state.fps };
        if forward { doc.apply_forward(&entry.command); } else { doc.apply_inverse(&entry.command); }
        Ok(())
    }

    fn one_step(&mut self, before: &klipp_timeline::LiftedPicture) {
        assert!(self.unapplied.is_empty(), "unapplied {:?}", self.unapplied);
        assert_eq!(self.state.active_history().undo_count(), 1, "one undo step");
        let after = self.picture();
        self.step(false).unwrap();
        assert_eq!(&self.picture(), before, "undo gives back the picture");
        self.step(true).unwrap();
        assert_eq!(self.picture(), after, "redo gives back the edit");
    }
}

/// A keyed grade parameter is listed as a track, named by its node's type
/// and its label, addressed by its path through the grade group; picked,
/// a double-click on the graph keys the grade node there, one step.
#[test]
fn a_keyed_grade_parameter_is_a_curve_editor_track() {
    let mut run = Run::new(0.0).keyed("exposure", &[(0.5, 0.0), (1.5, 1.0)]);
    let path = run.path();
    assert_eq!(path.nodes.len(), 2, "[group, node]: {path:?}");
    assert!(run.clip().animated_grade_keys().iter().any(|(p, k, ty)| *p == path && k.as_str() == "exposure" && ty == "klipp.grade.corrector"));
    run.click_text("Corrector: Exposure").unwrap();
    let before = run.picture();
    let at = run.panel.screen_at(1.0, 0.5).expect("the graph is drawn");
    for _ in 0..2 {
        run.click(at, egui::PointerButton::Primary);
    }
    run.frame(vec![egui::Event::PointerGone]);
    let keys = run.keys("exposure");
    assert!(keys.len() == 3 && keys.iter().any(|k| (k.time_secs - 1.0).abs() < 0.02 && (k.value - 0.5).abs() < 0.1), "{keys:?}");
    run.one_step(&before);
}

/// "+ Track" lists the clip's nodes and their unkeyed parameters; picking
/// the Corrector's Exposure keys it at the playhead with the value it has
/// (0.7), one undo step, and the track is then listed and shown.
#[test]
fn a_track_starts_on_an_unkeyed_grade_parameter() {
    let mut run = Run::new(1.2);
    let before = run.picture();
    assert!(run.keys("exposure").is_empty());
    run.click_text("+ Track").unwrap();
    run.hover_text("Corrector").unwrap();
    run.click_text("Exposure").unwrap();
    let keys = run.keys("exposure");
    assert!(keys.len() == 1 && (keys[0].time_secs - 1.2).abs() < 1e-6 && (keys[0].value - 0.7).abs() < 1e-6, "{keys:?}");
    assert!(run.frame(Vec::new()).iter().any(|(t, _)| t == "Corrector: Exposure"), "the started track isn't listed");
    run.one_step(&before);
}

/// A grade key's menu (a right click on the key in the editor), in the
/// app's menu look: Step (Hold) holds the move into and out of it; Ease In
/// slows the move into it; each one step.
#[test]
fn a_grade_key_menu_holds_or_eases_it() {
    for (item, ease) in [("Step (Hold)", None), ("Ease In", Some(KeyEase::In))] {
        let mut run = Run::new(0.0).keyed("exposure", &[(0.5, 0.0), (1.0, 1.0), (1.5, 0.0)]);
        run.click_text("Corrector: Exposure").unwrap();
        let before = run.picture();
        let key = run.panel.screen_at(1.0, 1.0).expect("the graph is drawn");
        run.click(key, egui::PointerButton::Secondary);
        run.click_text(item).unwrap_or_else(|e| panic!("{item}: {e}"));
        let mut want = vec![Keyframe { time_secs: 0.5, value: 0.0, interpolation: Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }, Keyframe { time_secs: 1.0, value: 1.0, interpolation: Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }, Keyframe { time_secs: 1.5, value: 0.0, interpolation: Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }];
        match ease {
            None => klipp_core::keyframe::set_interpolation_at(&mut want, 1, Interpolation::Step),
            Some(side) => klipp_core::keyframe::ease_key(&mut want, 1, side),
        }
        assert_eq!(run.keys("exposure"), want, "{item}");
        run.one_step(&before);
    }
}

