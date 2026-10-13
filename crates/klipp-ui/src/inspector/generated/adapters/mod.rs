//! The effect behaviour that isn't parameter editing, re-hosted for the
//! generated stack (M3 step 6): what an effect's presentation names by
//! `extra_ui`. An adapter draws above and below the effect's rows and
//! answers a row's edit with the side command today's tab sends (a
//! re-smooth, a re-gain). The drawing is the effects tab's own, called
//! from one place, so the two can't drift.

mod analysis;
mod frei0r;
mod lut_file;
mod media_item;
mod quiet;

pub use frei0r::Frei0rEditor;
pub use lut_file::LutFileEditor;
pub use media_item::MediaItemEditor;

use super::{node_rows, NodeRows, RowCtx, RowOutcome};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::Clip;

/// What an adapter reads and where it pushes.
pub struct AdapterCtx<'a> {
    pub theme: &'a KlippTheme,
    pub clip: &'a Clip,
    pub state: &'a AppState,
    pub cmds: &'a mut Vec<AppCommand>,
}

/// An effect's specialist behaviour, around its generated rows.
pub trait Adapter: Sync {
    /// Above the rows: an analysis's state and buttons.
    fn before_rows(&self, _ui: &mut egui::Ui, _ctx: &mut AdapterCtx) {}
    /// After the row for `key` did `outcome`: a side command it implies.
    fn after_row(&self, _ctx: &mut AdapterCtx, _key: &str, _outcome: RowOutcome) {}
    /// Below the rows: a note.
    fn after_rows(&self, _ui: &mut egui::Ui, _ctx: &mut AdapterCtx) {}
}

/// The adapters, by the name an effect's presentation gives (`extra_ui`).
static ADAPTERS: &[(&str, &dyn Adapter)] = &[
    ("deflicker", &analysis::Deflicker),
    ("video_stabilization", &analysis::Stabilization),
    ("temporal_denoise", &quiet::TemporalDenoise),
    // Their rows say all there is: a speed ramp retimes the clip when its
    // values change, a focus blur is set by a title's roll (its notice
    // says so), a colour fade's and a corner pin's other editing is in
    // the Video tab and the viewer.
    ("speed_ramp", &quiet::Nothing),
    ("focus_blur", &quiet::Nothing),
    ("color_fade", &quiet::Nothing),
    ("corner_pin", &quiet::Nothing),
];

/// The adapter named `id`, if any.
pub fn adapter(id: &str) -> Option<&'static dyn Adapter> {
    ADAPTERS.iter().find(|(name, _)| *name == id).map(|(_, a)| *a)
}

/// An effect's card body in the generated stack: its notice, its adapter
/// around its rows, and the side commands its rows' edits imply.
pub fn effect_card(ui: &mut egui::Ui, row: &mut RowCtx, rows: &NodeRows, ty: &klipp_effects::registry::EffectType, clip: &Clip, state: &AppState) {
    super::presets::preset_row(ui, row, ty, state);
    let tp = ty.type_presentation();
    let theme = row.theme;
    if let Some(notice) = tp.notice {
        ui.label(egui::RichText::new(notice).size(10.0).color(theme.text_dim));
    }
    let adapter = tp.extra_ui.and_then(adapter);
    if let Some(a) = adapter {
        // In a scope, which counts as one widget however much it draws, so
        // a status that changes mid-drag (an analysis's result drawing its
        // graph) leaves the rows below their ids and a held drag.
        ui.scope(|ui| a.before_rows(ui, &mut AdapterCtx { theme, clip, state, cmds: row.cmds }));
    }
    let outcomes = node_rows(ui, row, rows);
    if let Some(a) = adapter {
        let mut ctx = AdapterCtx { theme, clip, state, cmds: row.cmds };
        for (key, outcome) in outcomes {
            a.after_row(&mut ctx, key, outcome);
        }
        a.after_rows(ui, &mut ctx);
    }
}

#[cfg(test)]
mod tests {
    /// Every adapter an effect names is here, and nothing else is.
    #[test]
    fn every_named_adapter_is_registered() {
        let named: std::collections::BTreeSet<&str> = klipp_effects::registry::presentation::types().filter_map(|e| e.pres.extra_ui).collect();
        let registered: std::collections::BTreeSet<&str> = super::ADAPTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(named, registered);
    }
}
