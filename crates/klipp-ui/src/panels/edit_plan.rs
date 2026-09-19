//! The Edit Plan window: the paper edit an agent proposed over the
//! assistant socket, read here by the person whose word it waits on.
//! Approve records the approval as one undo step; Build lowers the
//! plan to ops, resolves them against the timeline as it stands, and
//! applies them as one undo step — the same path the socket's
//! build_edit_plan takes, so what an agent would build is what the
//! button builds.

use crate::state::AppState;
use crate::theme::KlippTheme;

/// What the person asked of the plan this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanAction {
    None,
    Approve,
    Build,
    Discard,
}

pub fn show_edit_plan(ctx: &egui::Context, state: &AppState, theme: &KlippTheme, open: &mut bool) -> PlanAction {
    let mut action = PlanAction::None;
    let seq = state.sequence();
    let target = seq.brief.as_ref().and_then(|b| b.target_secs);
    egui::Window::new("Edit Plan")
        .open(open)
        .default_size([460.0, 480.0])
        .min_width(280.0)
        .min_height(160.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let Some(plan) = seq.edit_plan.as_ref() else {
                ui.label(egui::RichText::new("No edit plan yet.").color(theme.text_secondary));
                ui.label(egui::RichText::new("An assistant proposes one from the footage and the brief (set_edit_plan over the klipp-mcp tools); it appears here to be read and approved before anything is placed.").small().color(theme.text_secondary));
                return;
            };
            let status = if plan.approved { "Approved" } else { "Awaiting your approval" };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(status).strong());
                ui.label(egui::RichText::new(format!("{} segments, {:.1} s{}", plan.segments.len(), plan.duration_secs(), target.map(|t| format!(" (the brief asks for {t:.0} s)")).unwrap_or_default())).small().color(theme.text_secondary));
            });
            ui.horizontal(|ui| {
                if !plan.approved && ui.add(egui::Button::new("Approve")).on_hover_text("Record your approval, as one undo step. Nothing is placed yet.").clicked() {
                    action = PlanAction::Approve;
                }
                if ui.add_enabled(plan.approved, egui::Button::new("Build")).on_hover_text(if plan.approved { "Place every segment, title and join after what is on the timeline, as one undo step." } else { "Approve the plan first." }).clicked() {
                    action = PlanAction::Build;
                }
                if ui.add(egui::Button::new("Discard")).on_hover_text("Remove the plan, as one undo step. The timeline is not touched.").clicked() {
                    action = PlanAction::Discard;
                }
            });
            if let Some(note) = &state.edit_plan_note {
                ui.label(egui::RichText::new(note).small().color(theme.text_secondary));
            }
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(plan.text(target)).monospace()).wrap());
            });
        });
    action
}
