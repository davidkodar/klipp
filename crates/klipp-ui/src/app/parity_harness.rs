//! Shared headless inspector driver. Families use the same pointer/key
//! events and command application boundary for both inspector views.

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_effects::BuiltinEffect;

#[test]
fn a_menu_edit_can_be_undone_and_redone_through_the_shared_driver() {
    let presets = crate::effect_presets::PresetManager::with_user_dir(std::path::PathBuf::from(
        "/nonexistent/klipp-presets",
    ));
    let preset = presets
        .presets_for_type("klipp.fx.gaussian_blur")
        .into_iter()
        .find(|p| p.is_builtin)
        .expect("blur preset");
    for generated in [false, true] {
        let mut run = Run::new(
            fixture(&BuiltinEffect::GaussianBlur { radius: 3.125 }),
            generated,
        );
        let before = run.clip().picture_v2().unwrap();
        assert!(!run.step(false), "new fixture has no undo step");
        assert!(run.click_last("Select preset..."));
        assert!(run.click_last(&preset.name));
        let edited = run.clip().picture_v2().unwrap();
        assert_ne!(
            edited, before,
            "the menu selection actually edits the picture"
        );
        assert!(run.step(false));
        assert_eq!(run.clip().picture_v2().unwrap(), before);
        assert!(run.step(true));
        assert_eq!(run.clip().picture_v2().unwrap(), edited);
        assert!(!run.step(true), "no extra redo step");
        assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    }
}

#[test]
fn undo_during_a_path_drag_closes_the_drag_before_taking_its_step() {
    use klipp_graph_format::path::{NodePath, Root};
    let mut run = Run::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }), true);
    let before = run.clip().picture_v2().unwrap();
    let node = before
        .picture
        .graph
        .nodes
        .iter()
        .find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur"))
        .unwrap()
        .id;
    let addr = NodePath::new(Root::Clip { clip: run.clip.0 }, [node]);
    run.apply(AppCommand::BeginParamEditAt {
        addr: addr.clone(),
        key: "radius".into(),
    });
    run.apply(AppCommand::SetParamAt {
        addr,
        key: "radius".into(),
        value: klipp_graph_format::ParamValue::Float(9.0),
    });
    let edited = run.clip().picture_v2().unwrap();
    assert_ne!(edited, before);
    assert!(run.state.any_drag_in_progress());
    assert!(run.step(false));
    assert!(!run.state.any_drag_in_progress());
    assert_eq!(run.clip().picture_v2().unwrap(), before);
    assert!(run.step(true));
    assert_eq!(run.clip().picture_v2().unwrap(), edited);
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
}

/// What the host would send on: recorded, never sent.
#[derive(Default)]
struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// The commands a view sends that neither the state nor the host applies,
/// and that the parity compares: the analysis side commands.
fn is_side(cmd: &AppCommand) -> bool {
    matches!(
        cmd,
        AppCommand::ResmoothStabilization { .. }
            | AppCommand::RegainDeflicker { .. }
            | AppCommand::AnalyzeStabilization { .. }
            | AppCommand::AnalyzeDeflicker { .. }
    )
}

/// The commands a view sends that set only what the viewer and the
/// timeline show, which the app's dispatch applies and no clip records.
fn is_view(cmd: &AppCommand) -> bool {
    matches!(
        cmd,
        AppCommand::SetInspectorMattePreviewTarget(_) | AppCommand::SetVideoRubberBandParam(_)
    )
}

/// One view of one clip, drawn and driven.
pub(super) struct Run {
    pub(super) state: AppState,
    pub(super) clip: ClipId,
    pub(super) ctx: egui::Context,
    pub(super) time: f64,
    pub(super) side: Vec<String>,
    pub(super) unapplied: Vec<String>,
    /// The generated view draws the clip's Video tab, not its stack: the
    /// effect is a clip property there (M3 step 13).
    pub(super) property: bool,
}

/// The one clip both views start from: a clone each, so their pictures
/// can be compared down to every id.
pub(super) fn fixture(effect: &BuiltinEffect) -> klipp_timeline::Clip {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::media(
        klipp_core::MediaId::new(),
        tc(0.0),
        tc(2.0),
        tc(0.0),
        tc(2.0),
    );
    clip.push_effect(effect.clone());
    clip
}

impl Run {
    /// Undo (`false`) or redo (`true`) through the document application
    /// path. Returns false when there is no history entry to apply.
    pub(super) fn step(&mut self, forward: bool) -> bool {
        self.state.before_dispatch(&if forward {
            AppCommand::Redo
        } else {
            AppCommand::Undo
        });
        let entry = if forward {
            self.state.active_history_mut().redo()
        } else {
            self.state.active_history_mut().undo()
        };
        let Some(entry) = entry else { return false };
        let mut view = klipp_document::apply::DocumentView {
            sequences: &mut self.state.sequences,
            active: self.state.active_sequence_idx,
            media_pool: &mut self.state.media_pool,
            fps: &mut self.state.fps,
        };
        if forward {
            view.apply_forward(&entry.command);
        } else {
            view.apply_inverse(&entry.command);
        }
        true
    }

    /// Click the last drawn occurrence, so an open parameter menu wins
    /// over the panel without relying on the effect-browser scroll anchor.
    pub(super) fn click_last(&mut self, text: &str) -> bool {
        let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
        let Some(at) = drawn
            .iter()
            .rev()
            .find(|(label, _)| label == text)
            .map(|(_, at)| *at)
        else {
            return false;
        };
        self.button(at, egui::PointerButton::Primary, true);
        self.button(at, egui::PointerButton::Primary, false);
        self.frame(Vec::new(), egui::Modifiers::NONE);
        true
    }

    pub(super) fn new(clip: klipp_timeline::Clip, generated: bool) -> Self {
        let id = clip.id;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.use_generated_stack = generated;
        Run {
            state,
            clip: id,
            ctx: egui::Context::default(),
            time: 0.0,
            side: Vec::new(),
            unapplied: Vec::new(),
            property: false,
        }
    }

    pub(super) fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0]
            .all_clips()
            .find(|c| c.id == self.clip)
            .unwrap()
    }

    /// One frame with `events`: the text drawn, at its centre. The commands
    /// the view sent are applied as the app applies them.
    pub(super) fn frame(
        &mut self,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> Vec<(String, egui::Pos2)> {
        self.time += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(520.0, 3000.0),
            )),
            time: Some(self.time),
            events,
            modifiers,
            ..Default::default()
        };
        let mut cmds = Vec::new();
        let (state, clip_id, property) = (&self.state, self.clip, self.property);
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let clip = state.sequences[0]
                    .all_clips()
                    .find(|c| c.id == clip_id)
                    .unwrap();
                let theme = KlippTheme::default();
                if state.use_generated_stack && property {
                    crate::inspector::video_tab::show(
                        ui,
                        &theme,
                        state,
                        Some((clip, 0.0, false)),
                        None,
                        &mut cmds,
                    );
                } else if state.use_generated_stack {
                    crate::inspector::stack_view::show(ui, &theme, state, clip, &mut cmds, 0.0);
                } else {
                    crate::inspector::effects_tab::show(
                        ui, &theme, state, clip, &mut cmds, &mut false, &mut false, 0.0,
                    );
                }
            });
        });
        for cmd in cmds {
            self.apply(cmd);
        }
        out.shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => {
                    Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0))
                }
                _ => None,
            })
            .collect()
    }

    pub(super) fn apply(&mut self, cmd: AppCommand) {
        self.state.before_dispatch(&cmd);
        if is_side(&cmd) {
            // An analysis the row asks for passes the dispatch boundary as in
            // the app, which must not close the row's drag.
            self.side.push(format!("{cmd:?}"));
            return;
        }
        // What the viewer and timeline show changes no clip; it passes the
        // dispatch boundary as in the app, which must not close a drag.
        if is_view(&cmd) {
            return;
        }
        // A refused path edit says so in a toast and changes nothing.
        self.state.toast = Default::default();
        if self.state.path_command(&cmd).is_some() {
            if let Some(why) = self.state.toast.message_for_test() {
                self.unapplied.push(format!("{cmd:?} refused: {why}"));
            }
            return;
        }
        if !(InspectorEditHost {
            state: &mut self.state,
            side: &Sink,
        })
        .apply(&cmd)
        {
            self.unapplied.push(format!("{cmd:?}"));
        }
    }

    /// The number drawn on the row labelled `label`: the first text right
    /// of it on its line that reads as a number.
    /// The row labelled `label`: its label and its number, where drawn.
    pub(super) fn row(&mut self, label: &str) -> Option<(egui::Pos2, egui::Pos2)> {
        let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
        // The label may be drawn twice (an effect named as its row): the
        // row's is the one with a number on its line.
        drawn
            .iter()
            .filter(|(s, _)| s == label)
            .find_map(|(_, at)| {
                drawn
                    .iter()
                    .filter(|(s, p)| {
                        (p.y - at.y).abs() < 6.0
                            && p.x > at.x
                            && s.trim_start_matches('-')
                                .starts_with(|c: char| c.is_ascii_digit())
                    })
                    .min_by(|a, b| a.1.x.total_cmp(&b.1.x))
                    .map(|(_, p)| (*at, *p))
            })
    }

    pub(super) fn button(&mut self, at: egui::Pos2, button: egui::PointerButton, pressed: bool) {
        self.frame(
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            egui::Modifiers::NONE,
        );
    }

    pub(super) fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            modifiers,
        );
    }
}

impl Run {
    /// Click the first text `label` drawn below the row named `row` (or
    /// anywhere when `row` is empty).
    pub(super) fn click_text(&mut self, row: &str, label: &str) -> bool {
        let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
        let y = if row.is_empty() {
            f32::MIN
        } else {
            match drawn.iter().find(|(s, _)| s == row) {
                Some((_, p)) => p.y,
                None => return false,
            }
        };
        let Some(at) = drawn
            .iter()
            .filter(|(s, p)| s == label && p.y > y)
            .min_by(|a, b| a.1.y.total_cmp(&b.1.y))
            .map(|(_, p)| *p)
        else {
            return false;
        };
        self.button(at, egui::PointerButton::Primary, true);
        self.button(at, egui::PointerButton::Primary, false);
        self.frame(Vec::new(), egui::Modifiers::NONE);
        true
    }

    /// Click `label` in an open menu: its last drawn occurrence (a menu is
    /// drawn over the panel), scrolling the menu down until it shows.
    pub(super) fn click_menu_item(&mut self, label: &str) -> bool {
        let mut scroll_at = None;
        for _ in 0..40 {
            let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
            if drawn.iter().rev().any(|(s, _)| s == label) {
                // Let a scroll settle, then find it again, so the click lands
                // where it is drawn.
                for _ in 0..10 {
                    self.frame(Vec::new(), egui::Modifiers::NONE);
                }
                let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
                let Some(at) = drawn
                    .iter()
                    .rev()
                    .find(|(s, _)| s == label)
                    .map(|(_, p)| *p)
                else {
                    continue;
                };
                self.button(at, egui::PointerButton::Primary, true);
                self.button(at, egui::PointerButton::Primary, false);
                self.frame(Vec::new(), egui::Modifiers::NONE);
                return true;
            }
            // Over the menu's own items: the first of its sorted names.
            let first = klipp_effects::registry::effect_types()
                .map(|t| t.desc.name)
                .min()
                .unwrap_or_default();
            let Some(at) = scroll_at.or_else(|| {
                drawn
                    .iter()
                    .rev()
                    .find(|(s, _)| s == first)
                    .map(|(_, p)| *p)
            }) else {
                return false;
            };
            scroll_at = Some(at);
            self.frame(
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -60.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                egui::Modifiers::NONE,
            );
        }
        false
    }

    /// The new view's eye on the row named `row`: left of its name.
    pub(super) fn click_eye(&mut self, row: &str) -> bool {
        let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
        let Some(at) = drawn
            .iter()
            .find(|(s, _)| s == row)
            .map(|(_, p)| egui::pos2(14.0, p.y))
        else {
            return false;
        };
        self.button(at, egui::PointerButton::Primary, true);
        self.button(at, egui::PointerButton::Primary, false);
        self.frame(Vec::new(), egui::Modifiers::NONE);
        true
    }

    /// An index command, as the old card's own control sends it.
    pub(super) fn send(&mut self, cmd: AppCommand) {
        self.apply(cmd);
    }
}
