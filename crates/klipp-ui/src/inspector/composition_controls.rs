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
}

impl CompositionControls {
    /// The section for `clip`, when its composition shows settings on Edit;
    /// a change made comes back.
    pub fn show(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip) -> Option<ControlChange> {
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
        if groups::published(&composition).is_empty() {
            return None;
        }
        let time = composition_time(state, clip)?;
        let mut change = None;
        crate::widgets::inspector_group::section_card(ui, "COMPOSITION", None, theme, |ui| {
            ui.add_space(4.0);
            egui::Frame::default().inner_margin(egui::Margin { left: 14, right: 14, top: 0, bottom: 0 }).show(ui, |ui| {
                let response = controls::ui(ui, &composition, time, &mut self.state);
                change = self.take(id, &bytes, response);
            });
            ui.add_space(4.0);
        });
        change
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
    let media_fps = state.media_pool.get(&clip.media_id()?)?.fps.as_f64();
    let fps = state.sequence().fps.as_f64();
    (fps > 0.0 && media_fps > 0.0).then(|| klipp_veylvik::clip_time::ClipTime { clip, fps, media_fps }.composition_time(state.playhead))
}
