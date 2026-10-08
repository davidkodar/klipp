//! Adapters with little or nothing beyond the rows.

use super::{Adapter, AdapterCtx};
use crate::inspector::effects_tab::TEMPORAL_DENOISE_CPU_NOTE;

/// An effect whose rows say all there is.
pub(super) struct Nothing;

impl Adapter for Nothing {}

/// Temporal Noise Reduction: its notice and its threshold's help are its
/// presentation's; this adds where it renders.
pub(super) struct TemporalDenoise;

impl Adapter for TemporalDenoise {
    fn after_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        ui.label(egui::RichText::new(TEMPORAL_DENOISE_CPU_NOTE).size(10.0).color(ctx.theme.text_dim));
    }
}
