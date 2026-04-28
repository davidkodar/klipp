//! egui rendering for the AI Tools panel.
//!
//! Visual layout (Side panel docked right by default; toggleable):
//!
//! ```text
//! ┌────────────────────────────────────────────┐
//! │ AI Tools                            [×]    │
//! ├────────────────────────────────────────────┤
//! │ ┌────────┬──────────┬─────────┬──────────┐ │
//! │ │Remove  │ Isolate  │ Detect  │  Track   │ │
//! │ │  BG    │  Object  │ Objects │  Object  │ │
//! │ └────────┴──────────┴─────────┴──────────┘ │
//! ├────────────────────────────────────────────┤
//! │ Model:  [Stub (placeholder)        ▾]      │
//! │ Caps:   POINT_PROMPT  AUTO_DETECT          │
//! │ Cached: ✓                                  │
//! ├────────────────────────────────────────────┤
//! │ [ Run on current frame ]                   │
//! ├────────────────────────────────────────────┤
//! │ Mask preview:                              │
//! │ ┌──────────────┐                           │
//! │ │  (preview    │  conf 0.92                │
//! │ │   thumbnail) │  inference 14 ms          │
//! │ └──────────────┘                           │
//! └────────────────────────────────────────────┘
//! ```
//!
//! The actual click-on-viewer plumbing (collecting `MaskPrompts`
//! from cursor events on the viewer canvas) is Track F's job. This
//! file only renders the panel + advances the dispatch state
//! machine. The "Run on current frame" button moves
//! Clicking "Run on current frame" sets `state.pending_run = true`;
//! the app dispatcher drains that, builds an `OwnedMaskRequest` from
//! the live viewer frame, and calls `spawn_inference`. The panel
//! never touches the worker thread directly.

use klipp_ml::{MaskCapabilities, ModelInfo};

use super::registry::{required_capability, AiToolsRegistry};
use super::{AiTask, AiToolsState, InferenceState};

/// Stateless renderer for the AI Tools panel. Holds nothing —
/// every frame the caller passes in the mutable state + the
/// (read-only) registry.
pub struct AiToolsPanel;

impl AiToolsPanel {
    /// Render the panel inside the supplied egui Ui (typically a
    /// `SidePanel` body the caller has already opened, or a
    /// `Window` central area). The caller decides docking.
    pub fn show(ui: &mut egui::Ui, state: &mut AiToolsState, registry: &AiToolsRegistry) {
        ui.heading("AI Tools");
        ui.add_space(4.0);

        // ── Task buttons ────────────────────────────────────────
        ui.horizontal_wrapped(|ui| {
            for task in AiTask::ALL {
                task_button(ui, *task, state, registry);
            }
        });

        ui.add_space(8.0);
        ui.separator();

        // ── Model picker + info card ────────────────────────────
        match state.active_task {
            Some(task) => model_section(ui, task, state, registry),
            None => {
                ui.label("Pick a task above to choose a model.");
            }
        }

        ui.add_space(8.0);
        ui.separator();

        // ── Run button ──────────────────────────────────────────
        run_section(ui, state, registry);

        ui.add_space(8.0);
        ui.separator();

        // ── Mask preview ────────────────────────────────────────
        preview_section(ui, state);
    }
}

fn task_button(
    ui: &mut egui::Ui,
    task: AiTask,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
) {
    let label = task.display_label();
    let has_model = registry.has_model_for(task);
    let active = state.active_task == Some(task);

    // Greyed out when no model satisfies the task; click is
    // suppressed and the tooltip nudges the user to install one.
    let response = ui.add_enabled(
        has_model,
        egui::Button::new(label)
            .min_size(egui::vec2(110.0, 44.0))
            .selected(active),
    );

    if !has_model {
        response.on_hover_text("No model installed — open Model Manager");
        return;
    }

    if response.clicked() {
        // Default the model dropdown to the first capable model
        // when switching tasks. Users can change it via the
        // dropdown afterwards.
        let first = registry
            .models_for(task)
            .first()
            .map(|m| m.id.clone());
        state.active_task = Some(task);
        state.selected_model_id = first;
    }
}

fn model_section(
    ui: &mut egui::Ui,
    task: AiTask,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
) {
    let models = registry.models_for(task);

    ui.horizontal(|ui| {
        ui.label("Model:");

        let current_label = state
            .selected_model_id
            .as_ref()
            .and_then(|id| registry.info(id))
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| "—".to_string());

        egui::ComboBox::from_id_salt("ai_tools_model_picker")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for m in &models {
                    let is_selected = state.selected_model_id.as_deref() == Some(m.id.as_str());
                    if ui
                        .selectable_label(is_selected, &m.display_name)
                        .clicked()
                    {
                        state.selected_model_id = Some(m.id.clone());
                    }
                }
            });
    });

    // Info card for the picked model.
    if let Some(info) = state
        .selected_model_id
        .as_ref()
        .and_then(|id| registry.info(id))
    {
        info_card(ui, info, task);
    } else {
        ui.label("(no model selected)");
    }
}

fn info_card(ui: &mut egui::Ui, info: &ModelInfo, task: AiTask) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(6.0))
        .show(ui, |ui| {
            ui.label(format!("Display name: {}", info.display_name));
            ui.label(format!("Id: {}", info.id));
            ui.horizontal(|ui| {
                ui.label("Capabilities:");
                for (flag, name) in capability_labels(info.capabilities) {
                    let required = required_capability(task);
                    let highlight = required.contains(flag);
                    let mut text = egui::RichText::new(name);
                    if highlight {
                        text = text.strong();
                    }
                    ui.label(text);
                }
            });
            // Cached/not-cached state will surface once we wire in
            // klipp_ml::ModelCache lookups; UI placeholder for now.
            ui.label("Cached: (status pending cache wire-up)");
        });
}

fn capability_labels(caps: MaskCapabilities) -> Vec<(MaskCapabilities, &'static str)> {
    let mut out = Vec::new();
    let pairs: &[(MaskCapabilities, &'static str)] = &[
        (MaskCapabilities::POINT_PROMPT, "POINT"),
        (MaskCapabilities::BOX_PROMPT, "BOX"),
        (MaskCapabilities::SCRIBBLE, "SCRIBBLE"),
        (MaskCapabilities::TEXT_PROMPT, "TEXT"),
        (MaskCapabilities::AUTO_DETECT, "AUTO"),
        (MaskCapabilities::TEMPORAL, "TEMPORAL"),
        (MaskCapabilities::BATCH, "BATCH"),
    ];
    for (flag, name) in pairs {
        if caps.contains(*flag) {
            out.push((*flag, *name));
        }
    }
    out
}

fn run_section(ui: &mut egui::Ui, state: &mut AiToolsState, registry: &AiToolsRegistry) {
    let can_run = state.active_task.is_some()
        && state
            .selected_model_id
            .as_ref()
            .map(|id| registry.info(id).is_some())
            .unwrap_or(false)
        && !matches!(state.inference, InferenceState::Running { .. });

    if ui
        .add_enabled(
            can_run,
            egui::Button::new("Run on current frame").min_size(egui::vec2(180.0, 32.0)),
        )
        .on_disabled_hover_text(disabled_run_reason(state))
        .clicked()
    {
        state.pending_run = true;
    }

    if matches!(state.inference, InferenceState::Running { .. })
        && ui.button("Cancel").clicked()
    {
        state.pending_cancel = true;
    }

    // Status line under the button so the user always sees state.
    match &state.inference {
        InferenceState::Idle => {
            ui.label(egui::RichText::new("Ready").weak());
        }
        InferenceState::Running { model_id, .. } => {
            ui.label(format!("Running on {}…", model_id));
        }
        InferenceState::Failed { error, .. } => {
            ui.colored_label(egui::Color32::from_rgb(220, 80, 80), error);
        }
        InferenceState::Done { response, .. } => {
            ui.label(format!("Done in {} ms", response.inference_ms));
        }
    }
}

fn disabled_run_reason(state: &AiToolsState) -> &'static str {
    if state.active_task.is_none() {
        "Pick a task first"
    } else if state.selected_model_id.is_none() {
        "Pick a model first"
    } else if matches!(state.inference, InferenceState::Running { .. }) {
        "Inference already running"
    } else {
        "Cannot run"
    }
}

fn preview_section(ui: &mut egui::Ui, state: &AiToolsState) {
    ui.label("Mask preview:");
    match &state.inference {
        InferenceState::Done { response, .. } => {
            if let Some(c) = response.candidates.first() {
                ui.label(format!(
                    "Confidence: {:.2}    Inference: {} ms    Mask: {}×{}",
                    c.confidence, response.inference_ms, c.mask.width, c.mask.height,
                ));
                if let Some(label) = &c.label {
                    ui.label(format!("Label: {}", label));
                }
                ui.label(egui::RichText::new("(viewer overlay shows the mask)").weak());
            } else {
                ui.label(egui::RichText::new("(empty response)").weak());
            }
        }
        _ => {
            ui.label(egui::RichText::new("(none yet)").weak());
        }
    }
}
