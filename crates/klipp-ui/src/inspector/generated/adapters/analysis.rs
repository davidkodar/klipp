//! The analysis-driven effects' adapters: Deflicker and Video
//! Stabilization measure the clip first, and an edit of the value the
//! measurement is smoothed by redoes only the smoothing.

use super::{Adapter, AdapterCtx};
use crate::inspector::effects_tab::{deflicker_status, stabilization_status, DEFLICKER_WINDOW_HINT};
use crate::inspector::generated::RowOutcome;
use klipp_core::AppCommand;

fn edited(outcome: RowOutcome) -> bool {
    matches!(outcome, RowOutcome::Edited | RowOutcome::Keyed | RowOutcome::Reset)
}

pub(super) struct Deflicker;

impl Adapter for Deflicker {
    fn before_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        deflicker_status(ui, ctx.theme, ctx.clip, ctx.state, ctx.cmds);
    }

    fn after_row(&self, ctx: &mut AdapterCtx, key: &str, outcome: RowOutcome) {
        // The measurement stands; only the smoothing over it is redone.
        if key == "window_secs" && edited(outcome) {
            ctx.cmds.push(AppCommand::RegainDeflicker { clip_id: ctx.clip.id });
        }
    }

    fn after_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        ui.label(egui::RichText::new(DEFLICKER_WINDOW_HINT).size(10.0).color(ctx.theme.text_dim));
    }
}

pub(super) struct Stabilization;

impl Adapter for Stabilization {
    fn before_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        stabilization_status(ui, ctx.theme, ctx.clip, ctx.state, ctx.cmds);
        ui.add_space(4.0);
    }

    fn after_row(&self, ctx: &mut AdapterCtx, key: &str, outcome: RowOutcome) {
        // Re-smoothing is cheap (no decode), so it follows the slider; it
        // needs a measurement to smooth.
        if key == "smoothing" && edited(outcome) && ctx.clip.stabilization_data.is_some() {
            ctx.cmds.push(AppCommand::ResmoothStabilization { clip_id: ctx.clip.id });
        }
    }
}
