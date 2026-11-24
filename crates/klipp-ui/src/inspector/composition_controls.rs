//! A composed clip's published Veylvik settings on Edit's inspector: the
//! ones its composition shows on Edit (ticked in Veylvik, or a group's or
//! recipe's controls), drawn by Veylvik's own panel at the playhead's
//! moment of the composition, so a title's words or a glow change without
//! opening the Veylvik page.
//!
//! The panel hands back Veylvik's commands; they are applied here to the
//! clip's composition and given to the app as its new bytes, for one
//! step of Klipp's history per change, or per drag.

use std::sync::Arc;

use klipp_core::ClipId;
use klipp_timeline::{Clip, CompositionId};
use klipp_veylvik::veylvik_document::{groups, Composition};
use klipp_veylvik::veylvik_editor::controls::{self, ControlsState};

use crate::state::AppState;
use klipp_core::AppCommand;
use crate::theme::KlippTheme;

/// A change made in the panel, for the app to take
/// ([`AppState::take_control_change`]).
pub struct ControlChange {
    pub composition: CompositionId,
    pub bytes: Arc<[u8]>,
    pub label: String,
    pub dragging: bool,
}

/// The panel's memory: the clip shown, its state, and the composition last
/// read from bytes (read again only when they change).
#[derive(Default)]
pub struct CompositionControls {
    clip: Option<ClipId>,
    state: ControlsState,
    read: Option<(Arc<[u8]>, Arc<Composition>)>,
    /// The step a held drag is named for.
    drag_label: String,
    /// A version being named: its place and the name so far.
    renaming: Option<(usize, String)>,
}

impl CompositionControls {
    /// The section for `clip`, when its composition shows settings on Edit;
    /// a change made comes back.
    pub fn show(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, cmds: &mut Vec<AppCommand>) -> Option<ControlChange> {
        let id = clip.composition?;
        let bytes = state.compositions.get(&id)?.clone();
        if self.clip != Some(clip.id) {
            *self = Self { clip: Some(clip.id), ..Self::default() };
        }
        let composition = match &self.read {
            Some((had, doc)) if Arc::ptr_eq(had, &bytes) || had[..] == bytes[..] => doc.clone(),
            _ => {
                let doc: Arc<Composition> = Arc::new(serde_json::from_slice(&bytes).ok()?);
                self.read = Some((bytes.clone(), doc.clone()));
                doc
            }
        };
        let shows = !groups::published(&composition).is_empty();
        let time = composition_time(state, clip);
        let mut change = None;
        crate::widgets::inspector_group::section_card(ui, "COMPOSITION", None, theme, |ui| {
            ui.add_space(4.0);
            egui::Frame::default().inner_margin(egui::Margin { left: 14, right: 14, top: 0, bottom: 0 }).show(ui, |ui| {
                self.versions(ui, clip, cmds);
                if let (true, Some(time)) = (shows, time) {
                    ui.add_space(6.0);
                    let response = controls::ui(ui, &composition, time, &mut self.state);
                    change = self.take(id, &bytes, response);
                }
            });
            ui.add_space(4.0);
        });
        change
    }

    /// The clip's versions: which one it shows, and new, duplicate,
    /// rename and remove.
    fn versions(&mut self, ui: &mut egui::Ui, clip: &Clip, cmds: &mut Vec<AppCommand>) {
        use klipp_core::VersionAction as A;
        let list = klipp_timeline::veylvik::versions(clip);
        let shown = list.iter().position(|v| Some(v.id) == clip.composition).unwrap_or(0);
        let mut send = |action| cmds.push(AppCommand::CompositionVersion { clip_id: clip.id, action });
        ui.horizontal(|ui| {
            ui.label("Version");
            let name = list.get(shown).map_or("", |v| v.name.as_str());
            egui::ComboBox::from_id_salt(("composition version", clip.id.0)).selected_text(name).show_ui(ui, |ui| {
                for (i, v) in list.iter().enumerate() {
                    if ui.selectable_label(i == shown, &v.name).clicked() && i != shown {
                        send(A::Switch(i));
                    }
                }
            });
            ui.menu_button("⋯", |ui| {
                if ui.button("New version").on_hover_text("A new composition of the clip's media alone, shown; the others kept").clicked() {
                    send(A::New);
                    ui.close();
                }
                if ui.button("Duplicate version").on_hover_text("A copy of the version shown, to try something on").clicked() {
                    send(A::Duplicate);
                    ui.close();
                }
                if ui.button("Rename version…").clicked() {
                    self.renaming = Some((shown, list.get(shown).map(|v| v.name.clone()).unwrap_or_default()));
                    ui.close();
                }
                if ui.add_enabled(list.len() > 1, egui::Button::new("Remove version")).clicked() {
                    send(A::Delete(shown));
                    ui.close();
                }
            });
        });
        if let Some((i, mut name)) = self.renaming.take() {
            let mut keep = true;
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0));
                if !edit.has_focus() && !edit.lost_focus() {
                    edit.request_focus();
                }
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter || ui.button("Rename").clicked() {
                    send(A::Rename(i, name.clone()));
                    keep = false;
                } else if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
            if keep {
                self.renaming = Some((i, name));
            }
        }
    }

    /// What the panel's `response` makes of composition `id`, now `bytes`:
    /// its commands applied, or the end of a drag that changed nothing
    /// more this frame.
    fn take(&mut self, id: CompositionId, bytes: &Arc<[u8]>, response: controls::ControlsResponse) -> Option<ControlChange> {
        if response.commands.is_empty() {
            // A drag ended on a frame of no change: its step closes here.
            let ended = !response.dragging && !self.drag_label.is_empty();
            return ended.then(|| ControlChange { composition: id, bytes: bytes.clone(), label: std::mem::take(&mut self.drag_label), dragging: false });
        }
        match klipp_veylvik::controls::apply(bytes, &response.commands) {
            Ok((now, label)) => {
                let label = format!("Veylvik: {label}");
                if response.dragging {
                    self.drag_label = label.clone();
                } else {
                    self.drag_label.clear();
                }
                Some(ControlChange { composition: id, bytes: now, label, dragging: response.dragging })
            }
            Err(e) => {
                tracing::warn!("a composition's setting could not be changed: {e}");
                None
            }
        }
    }
}

/// Where Klipp's playhead is in `clip`'s composition: its media time, as
/// the Veylvik page counts it.
fn composition_time(state: &AppState, clip: &Clip) -> Option<klipp_veylvik::Time> {
    let media_fps = state.document.media_pool.get(&clip.media_id()?)?.fps.as_f64();
    let fps = state.sequence().fps.as_f64();
    (fps > 0.0 && media_fps > 0.0).then(|| klipp_veylvik::clip_time::ClipTime { clip, fps, media_fps }.composition_time(state.playhead))
}
